//! X-RAY V2 (RFC 0015, the operator 2026-11-18): widgets you can arrange, install and ask an agent to change, ALL OF THEM DETERMINISTIC.
//!
//! The rule: a widget is a DECLARATION, never code and never a prompt: which catalog source it reads (a source that costs no AI call), with which typed arguments, and which renderer from a CLOSED list draws it.
//! The same declaration always draws the same picture. An agent changes X-ray only through these tools, and only by declarations the validator below accepts; everything is a saved version that can be undone.
//!
//! Where it lives: `<space>/.antiloki/xray/` - `widgets.json` (the person's and the agents' own widgets), `packages/<name>.json` (installed), `proposals/<name>.json` (proposed by an agent, waiting for the person),
//! `layouts.jsonl` (every version of every layout, newest last; a named view is a layout whose screen is `view:<name>`).
//!
//! Pure parts are tested here; `run_tool` / `http` only find the space and the connection.
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// The drawings a widget may ask for. A new one is a code change, reviewed like any other; a widget cannot bring its own.
pub const RENDERERS: [&str; 12] = ["text", "list", "number", "table", "bar", "line", "timeline", "treemap", "graph", "diff", "media", "component"];
pub const COLS: u32 = 12;
pub const TOOLS: [&str; 6] = ["antiloki_xray_widgets", "antiloki_xray_widget_save", "antiloki_xray_layout", "antiloki_xray_install", "antiloki_xray_preview", "antiloki_xray_slots"];
const OPTION_KEYS: [&str; 9] = ["path", "label", "value", "x", "y", "from", "to", "name", "limit"];
/// The parts of X-ray that stand alone (they need no file path): the components X-ray has today, as widgets of the board.
pub const COMPONENTS: [(&str, &str); 14] = [("overview", "Overview"), ("system-diagram", "System diagram"), ("import-graph", "Import graph"), ("data-stories", "Data stories"), ("constitution", "Constitution"), ("rules", "Quality rules"), ("coverage", "Coverage"), ("health-over-time", "Architecture over time"), ("review-history", "Review history"), ("working-tree", "Working tree"), ("git-graph", "Git graph"), ("authors", "Authors"), ("design", "Design"), ("telemetry", "Live signals")];

fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }
fn now_ms() -> u128 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) }

/// What the validator needs to know about one catalog source.
#[derive(Clone, Debug)]
pub struct SrcInfo { pub key: String, pub ai: bool, pub args: Vec<String> }
/// The catalog as the validator sees it.
pub fn catalog() -> Vec<SrcInfo> {
    let mut v: Vec<SrcInfo> = antiloki_rs::cockpit__helpers_context_catalog::CATALOG.iter().map(|c| SrcInfo { key: c.key.to_string(), ai: c.ai, args: c.args.iter().map(|a| a.0.to_string()).collect() }).collect();
    v.extend(LOCAL.iter().map(|l| SrcInfo { key: l.0.to_string(), ai: false, args: l.3.iter().map(|a| a.to_string()).collect() })); v
}
/// THE WORKSPACE'S OWN OUTPUTS (the operator 2026-11-19: "acme-mkt has no code, but it ran, it made videos and reports: none of that is on code?"): sources that read what a workspace HOLDS, code or not: the team runs and what they made, the kinds of file in the folder, its documents.
/// Deterministic, no AI, read from the folder and `.antiloki/team/`; (key, label, description, args).
pub const LOCAL: [(&str, &str, &str, &[&str]); 9] = [
    ("team_stats", "team totals", "the totals of this workspace's team work: runs, how many finished or failed, what they cost, what they made, how many documents (a JSON object)", &[]),
    ("slot", "a built-in part of X-ray", "one of X-ray's own components (overview, rules, constitution, coverage, git graph, authors, design, telemetry…), drawn by its built-in component; only with the renderer `component`", &[]),
    ("team_runs", "team runs", "every run the teams of this workspace made: run, team, ask, how it ended, what it cost (JSON rows)", &[]),
    ("team_outputs", "what the teams made", "the reports, pages, videos and pictures the runs left (artifacts and media), with size and where (JSON rows)", &[]),
    ("file_types", "kinds of file", "how many files of each kind the folder holds, outside .git, node_modules and build folders (JSON counts)", &[]),
    ("docs", "documents", "the markdown and text documents in the folder, with their size (JSON rows)", &["limit"]),
    ("book_findings", "findings of the book", "what the laws of the book find in this folder, any kind of file (code, copy, reports, plans, tables, pages, images): file, line, severity, law, message, fix (JSON rows; deterministic, no model)", &["limit"]),
    ("book_summary", "score of the book", "the book's verdict on this folder: score, axes, counts by severity, files judged, the digest that is the same for the same files and laws (a JSON object)", &[]),
    ("book_packs", "the laws of the book", "every pack and law of the book with its status, severity, detector and number of examples (JSON rows)", &[]),
];
/// The sources a widget may use: no AI call, listed with their arguments (the palette). Pure over the catalog.
pub fn palette(srcs: &[SrcInfo]) -> Vec<Value> {
    let mut out: Vec<Value> = antiloki_rs::cockpit__helpers_context_catalog::CATALOG.iter().filter(|c| !c.ai && srcs.iter().any(|x| x.key == c.key)).map(|c| json!({ "key": c.key, "label": c.label, "description": c.description, "args": c.args.iter().map(|a| json!({ "name": a.0, "about": a.1 })).collect::<Vec<_>>() })).collect();
    out.extend(LOCAL.iter().filter(|l| l.0 != "slot" && srcs.iter().any(|x| x.key == l.0)).map(|l| json!({ "key": l.0, "label": l.1, "description": l.2, "args": l.3.iter().map(|a| json!({ "name": a, "about": "how many (default 100)" })).collect::<Vec<_>>() }))); out
}

fn slug_ok(id: &str) -> bool { !id.is_empty() && id.len() <= 48 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')) }
fn scalar_ok(v: &Value) -> bool { match v { Value::String(t) => t.chars().count() <= 500, Value::Number(_) | Value::Bool(_) => true, _ => false } }

/// Is this declaration one X-ray may draw? Says why not, in words. Pure over the catalog.
pub fn validate_widget(w: &Value, srcs: &[SrcInfo]) -> Result<(), String> {
    let o = w.as_object().ok_or("a widget is an object")?;
    for k in o.keys() { if !["id", "title", "source", "args", "view", "options", "refresh", "base"].contains(&k.as_str()) { return Err(format!("`{k}` is not part of a widget: a widget is {{id, title, source, args, view, options, refresh}} and nothing else (it is a declaration, never code)")); } }
    let id = s(&w["id"]); if !slug_ok(&id) { return Err(format!("the id `{id}` must be 1 to 48 characters of a-z, 0-9, -, _ or .")); }
    let title = s(&w["title"]); if title.is_empty() || title.chars().count() > 80 { return Err("a widget needs a title of 1 to 80 characters".into()); }
    let source = s(&w["source"]);
    if source == "slot" { let view = s(&w["view"]); let name = s(&w["options"]["name"]); if view != "component" { return Err("a built-in part is drawn by the renderer `component`".into()); } if !SLOTS().iter().any(|x| x.0 == name) { return Err(format!("no built-in part `{name}`: they are {}", SLOTS().iter().map(|x| x.0).collect::<Vec<_>>().join(", "))); } if w.get("args").map(|a| a.as_object().map(|o| !o.is_empty()).unwrap_or(true)).unwrap_or(false) { return Err("a built-in part takes no arguments".into()); } return Ok(()); }
    let src = srcs.iter().find(|x| x.key == source).ok_or_else(|| format!("unknown source `{source}`: the palette is the catalog (antiloki_xray_widgets op sources)"))?;
    if src.ai { return Err(format!("the source `{source}` costs an AI call: X-ray widgets may only read sources that cost none")); }
    if let Some(a) = w.get("args") { let a = a.as_object().ok_or("`args` is an object")?; for (k, v) in a {
        if !src.args.contains(k) { return Err(format!("`{k}` is not an argument of `{source}` (its arguments: {})", if src.args.is_empty() { "none".to_string() } else { src.args.join(", ") })); }
        let ok = scalar_ok(v) || v.as_array().map(|l| l.len() <= 50 && l.iter().all(scalar_ok)).unwrap_or(false); if !ok { return Err(format!("the argument `{k}` must be a string, a number, a boolean or a short list of them")); } } }
    let view = s(&w["view"]); if !RENDERERS.contains(&view.as_str()) { return Err(format!("unknown renderer `{view}`: the renderers are {}", RENDERERS.join(", "))); }
    if let Some(op) = w.get("options") { let op = op.as_object().ok_or("`options` is an object")?; for (k, v) in op { if !OPTION_KEYS.contains(&k.as_str()) { return Err(format!("`{k}` is not an option (the options: {})", OPTION_KEYS.join(", "))); } if !scalar_ok(v) { return Err(format!("the option `{k}` must be a string, a number or a boolean")); } } }
    if let Some(r) = w.get("refresh") { if !matches!(r.as_str(), Some("manual") | Some("on-change")) { return Err("`refresh` is `manual` or `on-change`".into()); } }
    Ok(())
}

/// One cell of a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell { pub widget: String, pub x: u32, pub y: u32, pub w: u32, pub h: u32 }
pub fn cells_of(grid: &Value) -> Result<Vec<Cell>, String> {
    let list = grid.as_array().ok_or("`grid` is a list of {widget, x, y, w, h}")?;
    list.iter().map(|c| { let n = |k: &str| c[k].as_u64().filter(|v| *v <= 1000).map(|v| v as u32).ok_or_else(|| format!("`{k}` must be a whole number")); Ok(Cell { widget: s(&c["widget"]), x: n("x")?, y: n("y")?, w: n("w")?, h: n("h")? }) }).collect()
}
/// Is this grid one X-ray may draw: every widget known, once, inside the 12 columns, never overlapping another. Pure.
pub fn validate_layout(grid: &Value, known: &[String]) -> Result<Vec<Cell>, String> {
    let cells = cells_of(grid)?;
    for (i, c) in cells.iter().enumerate() {
        if !known.contains(&c.widget) { return Err(format!("no widget `{}`: it is not declared (antiloki_xray_widgets lists them)", c.widget)); }
        if c.w < 1 || c.h < 1 || c.w > COLS || c.h > 12 { return Err(format!("`{}` must be between 1 and {COLS} wide and 1 and 12 high", c.widget)); }
        if c.x + c.w > COLS { return Err(format!("`{}` runs past the right edge: x {} + w {} is more than {COLS} columns", c.widget, c.x, c.w)); }
        if cells[..i].iter().any(|d| d.widget == c.widget) { return Err(format!("`{}` is placed twice", c.widget)); }
        if let Some(d) = cells[..i].iter().find(|d| c.x < d.x + d.w && d.x < c.x + c.w && c.y < d.y + d.h && d.y < c.y + c.h) { return Err(format!("`{}` overlaps `{}`", c.widget, d.widget)); }
    }
    Ok(cells)
}
/// The first free place of `w` by `h` for another widget, scanning rows from the top. Pure.
pub fn free_spot(cells: &[Cell], w: u32, h: u32) -> (u32, u32) {
    for y in 0..200 { for x in 0..=(COLS.saturating_sub(w)) { if !cells.iter().any(|d| x < d.x + d.w && d.x < x + w && y < d.y + d.h && d.y < y + h) { return (x, y); } } }
    (0, cells.iter().map(|d| d.y + d.h).max().unwrap_or(0))
}

/// The base widgets: what X-ray shows today, each as a declaration like any other (the standard library: always there, never editable, never removable).
pub fn base_widgets() -> Vec<Value> {
    let comp = |slot: &str, title: &str| json!({ "id": format!("x-{slot}"), "title": title, "source": "slot", "view": "component", "args": {}, "options": { "name": slot }, "refresh": "on-change", "base": true });
    let mut out = base_data_widgets(); out.splice(0..0, COMPONENTS.iter().map(|(k, t)| comp(k, t))); out
}
fn base_data_widgets() -> Vec<Value> {
    let kpi = |id: &str, title: &str, path: &str| json!({ "id": id, "title": title, "source": "team_stats", "view": "number", "args": {}, "options": { "path": path }, "refresh": "on-change", "base": true });
    let w = |id: &str, title: &str, source: &str, view: &str| json!({ "id": id, "title": title, "source": source, "view": view, "args": {}, "options": {}, "refresh": "on-change", "base": true });
    vec![w("laws", "Laws", "laws", "list"), w("report", "Constitution report", "report", "text"), w("smells", "Smells", "smells", "text"), w("signals", "Code signals", "signals", "text"), w("digest", "Code digest", "digest", "text"),
         w("metrics", "Atlas metrics", "metrics", "text"), w("atlas-report", "Atlas report", "atlas_report", "text"), w("git", "Git", "git", "text"), w("history", "Repo history", "history", "list"), w("authors", "Authors", "authors", "text"),
         kpi("kpi-runs", "Runs", "runs"), kpi("kpi-done", "Finished", "done"), kpi("kpi-failed", "Failed", "failed"), kpi("kpi-spent", "Spent (USD)", "spent_usd"), kpi("kpi-made", "Things made", "made"), kpi("kpi-docs", "Documents", "documents"),
         w("team-runs", "Team runs", "team_runs", "table"), w("team-media", "Videos, pages and pictures the teams made", "team_outputs", "media"), w("team-outputs", "What the teams made", "team_outputs", "table"), w("file-types", "Kinds of file", "file_types", "bar"), w("docs", "Documents", "docs", "table")]
}

// ── slots: every part X-ray has today is a SLOT: replaceable by a declared widget, never deletable ──────
/// The slots: the parts of X-ray as they exist (the view names of antiloki_xray_open), each drawn by its built-in component until a widget is put in its place. `board` is the new board, not a slot.
#[allow(non_snake_case)]
pub fn SLOTS() -> Vec<(&'static str, &'static str)> { let mut v: Vec<(&'static str, &'static str)> = crate::cowork_xray::VIEWS.iter().filter(|(k, _)| *k != "board").map(|(k, v)| (*k, *v)).collect(); v.extend(SECTIONS); v }
/// The sections of the X-ray screen are slots too (the operator 2026-11-19): each can be replaced by a declared widget and restored, never deleted.
pub const SECTIONS: [(&str, &str); 9] = [("sec-made", "the screen's Made section: what the teams made, playing"), ("sec-runs", "the screen's Runs section"), ("sec-health", "the screen's Health section (the code analysis score and axes)"), ("sec-laws", "the screen's Laws hit section"), ("sec-hotspots", "the screen's Hotspots section"), ("sec-structure", "the screen's Structure section (graph smells)"), ("sec-authors", "the screen's Who wrote it section"), ("sec-files", "the screen's Files section"), ("sec-history", "the screen's History section")];
fn own_slots(root: &str) -> Map<String, Value> { read_json(&dir(root).join("slots.json")).and_then(|v| v.as_object().cloned()).unwrap_or_default() }
/// What the current theme's `xray.yaml` says, when there is one (a theme from a repository can carry a shape: declarations only, validated like any). None when the theme has none or it does not validate.
fn theme_shape(_root: &str) -> Option<Value> { let id = crate::themes::current()?; let d = theme_dir(&id, false)?; let text = std::fs::read_to_string(d.join("xray.yaml")).ok()?; shape_from_yaml(&text, &catalog()).ok() }
/// The slot overrides in force: the theme's, then the workspace's own on top of them.
pub fn slot_overrides(root: &str) -> Map<String, Value> { let mut m = theme_shape(root).and_then(|t| t["slots"].as_object().cloned()).unwrap_or_default(); for (k, v) in own_slots(root) { m.insert(k, v); } m }
fn slot_source(root: &str, slot: &str) -> &'static str { if own_slots(root).contains_key(slot) { "workspace" } else if theme_shape(root).map(|t| t["slots"].get(slot).is_some()).unwrap_or(false) { "theme" } else { "default" } }
/// The folder of a theme by id: antiloki's own store (`writable`: only that one), else Omarchy's. Ids are plain names.
pub fn theme_dir(id: &str, writable: bool) -> Option<PathBuf> {
    if !slug_ok(id) { return None; } let own = crate::themes::store_dir().join(id); if own.is_dir() { return Some(own); }
    if writable { return None; } [crate::themes::user_dir(), crate::themes::system_dir()].into_iter().map(|d| d.join(id)).find(|d| d.is_dir())
}

// ── the shape: widgets, slot replacements and screens, as one YAML (kept in a theme, like its colours) ─────
fn to_yaml(v: &Value) -> yaml_rust2::Yaml {
    use yaml_rust2::Yaml; match v { Value::Null => Yaml::Null, Value::Bool(b) => Yaml::Boolean(*b), Value::Number(n) => if let Some(i) = n.as_i64() { Yaml::Integer(i) } else { Yaml::Real(n.to_string()) }, Value::String(t) => Yaml::String(t.clone()),
        Value::Array(a) => Yaml::Array(a.iter().map(to_yaml).collect()), Value::Object(o) => Yaml::Hash(o.iter().map(|(k, x)| (Yaml::String(k.clone()), to_yaml(x))).collect()) }
}
fn from_yaml(y: &yaml_rust2::Yaml) -> Value {
    use yaml_rust2::Yaml; match y { Yaml::Null | Yaml::BadValue => Value::Null, Yaml::Boolean(b) => json!(b), Yaml::Integer(i) => json!(i), Yaml::Real(r) => r.parse::<f64>().ok().map(|f| json!(f)).unwrap_or(Value::Null), Yaml::String(t) => json!(t),
        Yaml::Array(a) => Value::Array(a.iter().map(from_yaml).collect()), Yaml::Hash(h) => Value::Object(h.iter().filter_map(|(k, v)| k.as_str().map(|k| (k.to_string(), from_yaml(v)))).collect()), _ => Value::Null }
}
/// The shape of a space as it is now: its own widgets, its slot replacements and the newest layout of every screen. Pure over the folder.
pub fn shape_of(root: &str) -> Value {
    let mut screens = Map::new(); let mut names: Vec<String> = versions(root).iter().map(|v| s(&v["screen"])).collect(); names.sort(); names.dedup(); for n in names { screens.insert(n.clone(), current_layout(root, &n)["grid"].clone()); }
    json!({ "xray": { "widgets": custom_widgets(root).into_iter().map(|mut w| { if let Some(o) = w.as_object_mut() { o.remove("custom"); } w }).collect::<Vec<_>>(), "slots": Value::Object(own_slots(root)), "screens": Value::Object(screens) } })
}
/// The shape as YAML text. Pure.
pub fn shape_to_yaml(shape: &Value) -> Result<String, String> {
    let mut out = String::from("# X-ray shape: widgets (declarations only), the slots they replace, the screens' grids. The built-in X-ray is what is left when this says nothing.\n");
    let mut em = yaml_rust2::YamlEmitter::new(&mut out); em.dump(&to_yaml(shape)).map_err(|e| format!("{e:?}"))?; out.push('\n'); Ok(out)
}
/// A shape read from YAML and checked: every widget a valid declaration, every slot a real one that names a declared widget, every screen a valid grid. Returns the `xray` object. Pure over the catalog.
pub fn shape_from_yaml(text: &str, srcs: &[SrcInfo]) -> Result<Value, String> {
    let docs = yaml_rust2::YamlLoader::load_from_str(text).map_err(|e| format!("not YAML: {e}"))?; let doc = from_yaml(docs.first().ok_or("the YAML is empty")?); let x = doc.get("xray").cloned().ok_or("the YAML has no `xray:` section")?;
    let o = x.as_object().ok_or("`xray` is a mapping")?; for k in o.keys() { if !["widgets", "slots", "screens"].contains(&k.as_str()) { return Err(format!("`{k}` is not part of an X-ray shape: it is widgets, slots and screens")); } }
    let ws = x["widgets"].as_array().cloned().unwrap_or_default(); let mut ids: Vec<String> = ws.iter().map(|w| s(&w["id"])).collect();
    for w in &ws { validate_widget(w, srcs).map_err(|e| format!("widget `{}`: {e}", s(&w["id"])))?; if w.get("base").is_some() { return Err("a shape cannot declare base widgets".into()); } }
    ids.extend(base_widgets().iter().map(|b| s(&b["id"])));
    if let Some(sl) = x["slots"].as_object() { for (k, v) in sl { if !SLOTS().iter().any(|y| y.0 == k.as_str()) { return Err(format!("no slot `{k}`")); } if !ids.contains(&s(v)) { return Err(format!("the slot `{k}` names `{}`, which is not declared", s(v))); } } }
    if let Some(sc) = x["screens"].as_object() { for (k, g) in sc { screen_ok(k)?; validate_layout(g, &ids).map_err(|e| format!("screen `{k}`: {e}"))?; } }
    Ok(json!({ "widgets": ws, "slots": x["slots"].clone(), "screens": x["screens"].clone() }))
}
/// Put a validated shape into a space: its widgets saved, its slots set, each screen a new layout version. Returns how many parts changed.
pub fn apply_shape(root: &str, srcs: &[SrcInfo], sh: &Value, by: &str) -> Result<usize, String> {
    let mut n = 0; for w in sh["widgets"].as_array().cloned().unwrap_or_default() { run_op(root, srcs, "widget_save", &json!({ "widget": w }), by)?; n += 1; }
    if let Some(sl) = sh["slots"].as_object() { let mut m = own_slots(root); for (k, v) in sl { m.insert(k.clone(), v.clone()); n += 1; } write_json(&dir(root).join("slots.json"), &Value::Object(m))?; }
    if let Some(sc) = sh["screens"].as_object() { for (k, g) in sc { set_layout(root, k, g, by, "from a shape")?; n += 1; } } Ok(n)
}

// ── the store ─────────────────────────────────────────────────────────────────────────────────────
fn dir(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("xray") }
fn read_json(p: &Path) -> Option<Value> { std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()) }
fn write_json(p: &Path, v: &Value) -> Result<(), String> { if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; } std::fs::write(p, serde_json::to_string_pretty(v).unwrap_or_default()).map_err(|e| e.to_string()) }
fn custom_widgets(root: &str) -> Vec<Value> { read_json(&dir(root).join("widgets.json")).and_then(|v| v["widgets"].as_array().cloned()).unwrap_or_default() }
fn packages(root: &str) -> Vec<Value> { let mut v: Vec<Value> = std::fs::read_dir(dir(root).join("packages")).into_iter().flatten().flatten().filter_map(|e| read_json(&e.path())).collect(); v.sort_by(|a, b| s(&a["name"]).cmp(&s(&b["name"]))); v }
fn proposals(root: &str) -> Vec<Value> { let mut v: Vec<Value> = std::fs::read_dir(dir(root).join("proposals")).into_iter().flatten().flatten().filter_map(|e| read_json(&e.path())).collect(); v.sort_by(|a, b| s(&a["name"]).cmp(&s(&b["name"]))); v }
/// Every declared widget with where it comes from: base, a package (ids prefixed by the package) or custom.
pub fn all_widgets(root: &str) -> Vec<Value> {
    let mut out = base_widgets();
    for p in packages(root) { for w in p["widgets"].as_array().cloned().unwrap_or_default() { let mut w = w; let id = format!("{}.{}", s(&p["name"]), s(&w["id"])); w["id"] = json!(id); w["package"] = p["name"].clone(); out.push(w); } }
    for mut w in custom_widgets(root) { w["custom"] = json!(true); out.push(w); }
    out
}
fn ids_of(ws: &[Value]) -> Vec<String> { ws.iter().map(|w| s(&w["id"])).collect() }
fn versions(root: &str) -> Vec<Value> { std::fs::read_to_string(dir(root).join("layouts.jsonl")).ok().map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default() }
fn append_version(root: &str, v: &Value) -> Result<(), String> {
    use std::io::Write; let p = dir(root).join("layouts.jsonl"); std::fs::create_dir_all(dir(root)).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p).map_err(|e| e.to_string())?; writeln!(f, "{}", v).map_err(|e| e.to_string())
}
/// The grid a screen starts with: the base widgets two to a row. Pure.
pub fn default_grid(screen: &str) -> Value {
    // THREE SCREENS, TIDY (the operator 2026-11-19: "just show pretty widgets, all deterministic"): the MAIN screen of a team is a dashboard (totals, what it made, its runs, the kinds of file, its documents); `-parts` holds X-ray's own components; `-data` the raw readings of the code widgets.
    let kind = if screen == "home" || screen.ends_with("-parts") { "parts" } else if screen.ends_with("-data") { "data" } else if screen.starts_with("team-") { "main" } else { return json!([]) };
    let base = base_widgets(); let is_comp = |id: &str| id.starts_with("x-");
    let ids: Vec<String> = match kind {
        "main" => ["kpi-runs", "kpi-done", "kpi-failed", "kpi-spent", "kpi-made", "kpi-docs", "team-media", "team-runs", "file-types", "docs"].iter().map(|x| x.to_string()).collect(),
        "parts" => base.iter().map(|w| s(&w["id"])).filter(|i| is_comp(i)).collect(),
        _ => ["laws", "report", "smells", "signals", "digest", "metrics", "atlas-report", "git", "history", "authors", "team-outputs"].iter().map(|x| x.to_string()).collect(),
    };
    let size = |id: &str| -> (u32, u32) { match id { x if x.starts_with("kpi-") => (2, 2), "team-media" => (12, 6), "team-runs" => (12, 4), "file-types" | "docs" => (6, 4), "team-outputs" => (12, 4), "x-overview" | "x-system-diagram" | "x-import-graph" | "x-design" => (12, 9), x if is_comp(x) => (6, 7), _ => (6, 4) } };
    let (mut x, mut y, mut row_h) = (0u32, 0u32, 0u32); let mut cells = vec![];
    for id in ids { let (w, h) = size(&id); if x + w > COLS { x = 0; y += row_h; row_h = 0; } cells.push(json!({ "widget": id, "x": x, "y": y, "w": w, "h": h })); x += w; row_h = row_h.max(h); }
    Value::Array(cells)
}
/// The newest version of a screen's layout, with widgets that are no longer declared dropped. `n` = 0 is the default, never saved.
pub fn current_layout(root: &str, screen: &str) -> Value {
    let known = ids_of(&all_widgets(root)); let vs = versions(root); let last = vs.iter().rev().find(|v| s(&v["screen"]) == screen);
    let grid = last.map(|v| v["grid"].clone()).unwrap_or_else(|| default_grid(screen)); let kept: Vec<Value> = grid.as_array().cloned().unwrap_or_default().into_iter().filter(|c| known.contains(&s(&c["widget"]))).collect();
    json!({ "screen": screen, "n": last.map(|v| v["n"].clone()).unwrap_or(json!(0)), "by": last.map(|v| v["by"].clone()).unwrap_or(json!("default")), "note": last.map(|v| v["note"].clone()).unwrap_or(json!("")), "grid": kept })
}
fn screen_ok(screen: &str) -> Result<(), String> { let rest = screen.strip_prefix("view:").unwrap_or(screen); if slug_ok(rest) { Ok(()) } else { Err("a screen is `home`, another name, or `view:<name>` (a-z, 0-9, -, _)".into()) } }
fn set_layout(root: &str, screen: &str, grid: &Value, by: &str, note: &str) -> Result<Value, String> {
    screen_ok(screen)?; let known = ids_of(&all_widgets(root)); validate_layout(grid, &known)?;
    let n = versions(root).iter().filter(|v| s(&v["screen"]) == screen).count() as u64 + 1;
    append_version(root, &json!({ "screen": screen, "n": n, "ts": now_ms() as u64, "by": by, "note": note, "grid": grid }))?; Ok(current_layout(root, screen))
}

/// A package is a list of declarations (nothing executable): every widget must pass the same validator a hand-made one does. Pure over the catalog.
pub fn validate_package(p: &Value, srcs: &[SrcInfo]) -> Result<(), String> {
    let o = p.as_object().ok_or("a package is an object")?; for k in o.keys() { if !["name", "version", "widgets", "layouts"].contains(&k.as_str()) { return Err(format!("`{k}` is not part of a package: a package is {{name, version, widgets, layouts?}} (declarations only)")); } }
    let name = s(&p["name"]); if !slug_ok(&name) || name.contains('.') { return Err("a package needs a name of a-z, 0-9, - or _".into()); }
    let ws = p["widgets"].as_array().ok_or("a package has a list of widgets")?; if ws.is_empty() || ws.len() > 40 { return Err("a package holds 1 to 40 widgets".into()); }
    let mut seen = std::collections::HashSet::new();
    for w in ws { validate_widget(w, srcs).map_err(|e| format!("widget `{}`: {e}", s(&w["id"])))?; if !seen.insert(s(&w["id"])) { return Err(format!("the widget id `{}` is used twice", s(&w["id"]))); } if w.get("base").is_some() { return Err("a package cannot declare base widgets".into()); } }
    if let Some(ls) = p.get("layouts") { for l in ls.as_array().ok_or("`layouts` is a list")? { let ids: Vec<String> = ws.iter().map(|w| s(&w["id"])).collect(); validate_layout(&l["grid"], &ids).map_err(|e| format!("layout `{}`: {e}", s(&l["screen"])))?; } }
    Ok(())
}

/// The stamp a source puts on its answer (`<!-- antiloki context · kind=… · generated=… -->`) is the clock, not the data: a widget drawn twice must give the same picture, so it is taken off. Pure.
pub fn strip_stamp(content: &str) -> String { content.lines().filter(|l| !l.trim_start().starts_with("<!-- antiloki context")).collect::<Vec<_>>().join("\n") }
fn kind_of_file(name: &str) -> &'static str { match name.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).as_deref() { Some("mp4" | "webm" | "mov" | "m4v") => "video", Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg") => "picture", Some("mp3" | "wav" | "ogg") => "sound", Some("md" | "txt") => "document", Some("html") => "page", Some("csv" | "json") => "data", _ => "file" } }
fn walk_files(dir: &Path, base: &Path, depth: usize, skip: &[&str], out: &mut Vec<(String, u64)>) {
    if depth > 8 || out.len() > 20_000 { return; }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() { let p = e.path(); let n = e.file_name().to_string_lossy().to_string();
        if p.is_dir() { if !skip.contains(&n.as_str()) && !n.starts_with('.') { walk_files(&p, base, depth + 1, skip, out); } } else if let Ok(m) = e.metadata() { out.push((p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/"), m.len())); } }
}
/// What one local source answers: (content, is_json). Pure over the folder.
pub fn local_read(root: &str, key: &str, args: &Value) -> Result<(String, bool), String> {
    let r = Path::new(root); let limit = args["limit"].as_u64().unwrap_or(100).min(500) as usize;
    match key {
        "team_runs" => { let mut rows: Vec<Value> = std::fs::read_dir(r.join(".antiloki").join("team")).into_iter().flatten().flatten().filter_map(|e| { let st = read_json(&e.path().join("state.json"))?; let state = &st["state"]; let status = state["status"]["status"].as_str().unwrap_or("").to_string();
                Some(json!({ "run": s(&st["id"]), "team": s(&st["team"]), "ask": s(&st["ask"]).chars().take(90).collect::<String>(), "status": status, "spent_usd": state["spent_usd"].as_f64().map(|x| (x * 100.0).round() / 100.0).unwrap_or(0.0), "started": state["started_ms"].as_u64().unwrap_or(0) })) }).collect();
            rows.sort_by(|a, b| b["started"].as_u64().cmp(&a["started"].as_u64())); rows.truncate(limit); Ok((Value::Array(rows).to_string(), true)) }
        // THE BOOK (RFC 0018): the same deterministic judge that gates a team feeds the widgets
        "book_findings" | "book_summary" | "book_packs" => {
            let (book, _) = crate::lanes::load_book(root);
            if key == "book_packs" { let rows: Vec<Value> = book.iter().flat_map(|p| p.laws.iter().map(move |l| json!({ "pack": p.id, "law": l.key, "title": l.title, "status": l.status, "severity": l.severity, "detector": l.detector, "axis": l.axis, "examples": l.fixtures.pass.len() + l.fixtures.fail.len() }))).collect(); return Ok((Value::Array(rows).to_string(), true)); }
            let rep = crate::lanes::judge(&book, &crate::lanes::collect(r, &[], 600), &crate::lanes::Opts::default());
            if key == "book_summary" { return Ok((json!({ "score": rep["score"], "axes": rep["axes"], "counts": rep["counts"], "files": rep["artifacts"].as_array().map_or(0, |a| a.len()), "laws_run": rep["laws_run"], "passed": rep["gate"]["passed"], "skipped": rep["gate"]["skipped"], "digest": rep["digest"] }).to_string(), true)); }
            let rows: Vec<Value> = rep["findings"].as_array().into_iter().flatten().filter(|f| f["advisory"] != json!(true)).take(limit).map(|f| json!({ "path": f["path"], "line": f["line"], "severity": f["severity"], "law": f["law"], "message": f["message"], "fix": f["fix"] })).collect(); Ok((Value::Array(rows).to_string(), true))
        }
        "team_stats" => { let runs: Vec<Value> = serde_json::from_str(&local_read(root, "team_runs", &json!({ "limit": 500 }))?.0).unwrap_or_default(); let outs: Vec<Value> = serde_json::from_str(&local_read(root, "team_outputs", &json!({ "limit": 500 }))?.0).unwrap_or_default(); let docs: Vec<Value> = serde_json::from_str(&local_read(root, "docs", &json!({ "limit": 500 }))?.0).unwrap_or_default();
            let n = |st: &str| runs.iter().filter(|r| r["status"] == json!(st)).count(); let spent: f64 = runs.iter().map(|r| r["spent_usd"].as_f64().unwrap_or(0.0)).sum();
            Ok((json!({ "runs": runs.len(), "done": n("done"), "failed": n("blocked") + n("stopped"), "spent_usd": (spent * 100.0).round() / 100.0, "made": outs.len(), "videos": outs.iter().filter(|o| matches!(o["kind"].as_str(), Some("video" | "page"))).count(), "documents": docs.len() }).to_string(), true)) }
        "team_outputs" => { let mut rows: Vec<Value> = vec![]; for e in std::fs::read_dir(r.join(".antiloki").join("team")).into_iter().flatten().flatten() { let run = e.file_name().to_string_lossy().to_string(); if !e.path().is_dir() { continue; }
                for sub in ["artifacts", "media"] { let mut v = vec![]; walk_files(&e.path().join(sub), &e.path().join(sub), 0, &[], &mut v); for (name, bytes) in v { rows.push(json!({ "run": run, "kind": kind_of_file(&name), "name": name, "kb": ((bytes as f64) / 1024.0 * 10.0).round() / 10.0, "from": sub })); } } }
            rows.sort_by(|a, b| s(&b["run"]).cmp(&s(&a["run"]))); rows.truncate(limit.max(200)); Ok((Value::Array(rows).to_string(), true)) }
        "file_types" => { let mut v = vec![]; walk_files(r, r, 0, &["node_modules", "target", "dist", "build", "venv", ".venv", "__pycache__"], &mut v); let mut m: std::collections::BTreeMap<String, u64> = Default::default();
            for (n, _) in v { let ext = n.rsplit_once('.').filter(|x| !x.0.is_empty() && x.1.len() <= 8).map(|x| x.1.to_ascii_lowercase()).unwrap_or_else(|| "(none)".to_string()); *m.entry(ext).or_default() += 1; }
            let mut top: Vec<(String, u64)> = m.into_iter().collect(); top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0))); top.truncate(20); Ok((Value::Object(top.into_iter().map(|(k, n)| (k, json!(n))).collect()).to_string(), true)) }
        "docs" => { let mut v = vec![]; walk_files(r, r, 0, &["node_modules", "target", "dist", "build"], &mut v); let mut rows: Vec<Value> = v.into_iter().filter(|(n, _)| matches!(n.rsplit_once('.').map(|x| x.1.to_ascii_lowercase()).as_deref(), Some("md" | "txt" | "mdx"))).map(|(n, b)| json!({ "document": n, "kb": ((b as f64) / 1024.0 * 10.0).round() / 10.0 })).collect(); rows.sort_by(|a, b| s(&a["document"]).cmp(&s(&b["document"]))); rows.truncate(limit); Ok((Value::Array(rows).to_string(), true)) }
        other => Err(format!("no local source `{other}`")),
    }
}
// ── drawing: the data a renderer needs, from a source's output ─────────────────────────────────────
fn path_get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> { if path.is_empty() { return Some(v); } path.split('.').try_fold(v, |c, k| match c { Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)), Value::Object(o) => o.get(k), _ => None }) }
fn text_of(v: &Value) -> String { match v { Value::String(t) => t.clone(), Value::Null => String::new(), other => other.to_string() } }
fn opt(options: &Value, k: &str, d: &str) -> String { let t = s(&options[k]); if t.is_empty() { d.to_string() } else { t } }
/// The data a renderer draws from a source's answer (`json`: the answer is JSON, else text). Pure: the same answer and options always give the same data; an answer that does not fit the renderer says so.
pub fn shape(view: &str, options: &Value, content: &str, json_out: bool) -> Result<Value, String> {
    let parsed: Option<Value> = if json_out { serde_json::from_str(content).ok() } else { None };
    let at = |p: &str| -> Option<Value> { parsed.as_ref().and_then(|v| path_get(v, p).cloned()) }; let path = opt(options, "path", "");
    let limit = options["limit"].as_u64().unwrap_or(100).min(500) as usize; let lines = || -> Vec<String> { content.lines().map(|l| l.trim_end().to_string()).filter(|l| !l.trim().is_empty()).collect() };
    let need_json = |what: &str| -> Result<Value, String> { at(&path).ok_or_else(|| format!("this source's answer is not JSON at `{}`: it cannot be drawn as {what}", if path.is_empty() { "the top" } else { &path })) };
    match view {
        "text" => Ok(json!({ "text": content.chars().take(60_000).collect::<String>() })),
        "list" => { let items: Vec<String> = match at(&path) { Some(Value::Array(a)) => a.iter().map(|x| if x.is_object() { ["name", "title", "path", "id", "key"].iter().find_map(|k| x[*k].as_str()).map(|t| t.to_string()).unwrap_or_else(|| x.to_string()) } else { text_of(x) }).collect(), Some(Value::Object(o)) => o.keys().cloned().collect(), _ => lines() }; Ok(json!({ "items": items.into_iter().take(limit).collect::<Vec<_>>() })) }
        "number" => { let v = match at(&path) { Some(Value::Number(n)) => n.as_f64(), Some(Value::Array(a)) => Some(a.len() as f64), Some(Value::Object(o)) => Some(o.len() as f64), Some(Value::String(t)) => t.trim().parse().ok(), _ => if parsed.is_none() { Some(lines().len() as f64) } else { None } }; let v = v.ok_or("there is no number at that path")?; Ok(json!({ "value": v, "label": opt(options, "label", "") })) }
        "table" => { let rows = need_json("a table")?; let rows = rows.as_array().ok_or("a table needs a list of rows")?; let first = rows.iter().find_map(|r| r.as_object()).ok_or("a table needs rows that are objects")?; let cols: Vec<String> = first.keys().take(8).cloned().collect();
            Ok(json!({ "columns": cols, "rows": rows.iter().take(limit).map(|r| cols.iter().map(|c| json!(text_of(&r[c.as_str()]).chars().take(120).collect::<String>())).collect::<Vec<_>>()).collect::<Vec<_>>() })) }
        "bar" | "treemap" => { let data = need_json(if view == "bar" { "bars" } else { "a treemap" })?; let (lk, vk) = (opt(options, "label", "name"), opt(options, "value", "value")); let mut out: Vec<Value> = vec![];
            match &data { Value::Object(o) => for (k, v) in o { if let Some(n) = v.as_f64() { out.push(json!({ "label": k, "value": n })); } }, Value::Array(a) => for r in a { if let (Some(l), Some(n)) = (r.get(&lk).map(text_of), r.get(&vk).and_then(|x| x.as_f64())) { out.push(json!({ "label": l, "value": n })); } }, _ => {} }
            if out.is_empty() { return Err(format!("nothing to draw: it needs an object of numbers or a list with `{lk}` and `{vk}`")); } out.truncate(limit.min(60)); { let mut m = Map::new(); m.insert(if view == "bar" { "bars" } else { "cells" }.to_string(), Value::Array(out)); Ok(Value::Object(m)) } }
        "line" => { let data = need_json("a line")?; let a = data.as_array().ok_or("a line needs a list")?; let (xk, yk) = (opt(options, "x", "x"), opt(options, "y", "y")); let pts: Vec<Value> = a.iter().enumerate().filter_map(|(i, r)| if let Some(n) = r.as_f64() { Some(json!({ "x": i, "y": n })) } else { r.get(&yk).and_then(|y| y.as_f64()).map(|y| json!({ "x": r.get(&xk).cloned().unwrap_or(json!(i)), "y": y })) }).collect(); if pts.len() < 2 { return Err("a line needs at least two points".into()); } Ok(json!({ "points": pts.into_iter().take(limit.max(2)).collect::<Vec<_>>() })) }
        "timeline" => { let data = need_json("a timeline")?; let a = data.as_array().ok_or("a timeline needs a list")?; let (tk, lk) = (opt(options, "x", "date"), opt(options, "label", "subject")); let ev: Vec<Value> = a.iter().filter_map(|r| { let t = r.get(&tk).or_else(|| r.get("t")).or_else(|| r.get("ts")).map(text_of)?; Some(json!({ "t": t, "label": r.get(&lk).map(text_of).unwrap_or_default().chars().take(120).collect::<String>() })) }).take(limit).collect(); if ev.is_empty() { return Err(format!("no entry has a `{tk}` to place it in time")); } Ok(json!({ "events": ev })) }
        "graph" => { let data = need_json("a graph")?; let a = data.as_array().or_else(|| data["edges"].as_array()).ok_or("a graph needs a list of edges")?; let (fk, tk) = (opt(options, "from", "from"), opt(options, "to", "to")); let e: Vec<Value> = a.iter().filter_map(|r| Some(json!({ "from": text_of(r.get(&fk)?), "to": text_of(r.get(&tk)?) }))).take(limit.min(300)).collect(); if e.is_empty() { return Err(format!("no edge has `{fk}` and `{tk}`")); } Ok(json!({ "edges": e })) }
        "component" => { let name = opt(options, "name", ""); if !SLOTS().iter().any(|x| x.0 == name) { return Err(format!("no built-in part `{name}`")); } Ok(json!({ "name": name })) }
        "media" => { let data = need_json("media")?; let a = data.as_array().ok_or("media needs a list of rows with run, kind and name")?; let items: Vec<Value> = a.iter().filter(|r| s(&r["from"]) == "media" && matches!(s(&r["kind"]).as_str(), "video" | "picture" | "sound" | "page")).map(|r| json!({ "run": s(&r["run"]), "kind": s(&r["kind"]), "name": s(&r["name"]), "file": format!("kept/{}", s(&r["name"])) })).take(limit.min(60)).collect();
            if items.is_empty() { return Err("no video, picture, sound or page was made by the runs of this workspace yet".into()); } Ok(json!({ "items": items })) }
        "diff" => Ok(json!({ "lines": content.lines().take(2000).map(|l| json!({ "kind": if l.starts_with("@@") { "hunk" } else if l.starts_with('+') && !l.starts_with("+++") { "add" } else if l.starts_with('-') && !l.starts_with("---") { "del" } else { "ctx" }, "text": l })).collect::<Vec<_>>() })),
        other => Err(format!("unknown renderer `{other}`")),
    }
}

// ── the operations (pure over the folder: the same for the screen and the agents) ──────────────────
/// One operation on a space's X-ray store. `by` is `person` or `agent`. Pure over the folder and the catalog; data and preview are drawn by the caller (they need the database).
pub fn run_op(root: &str, srcs: &[SrcInfo], op: &str, a: &Value, by: &str) -> Result<Value, String> {
    let agent = by == "agent";
    match op {
        "sources" => Ok(json!({ "ok": true, "sources": palette(srcs), "renderers": RENDERERS })),
        "tree_files" => Ok(json!({ "ok": true, "files": tree_files_of(root) })),
        "trees" => { // the trees of this space's threads: the work of the teams, which X-ray can read as spaces of their own
            let trees: Vec<Value> = crate::thread_tree::trees_of(root).into_iter().map(|(thread, p)| { let files = crate::thread_tree::git(&["ls-files"], &p).map(|o| o.lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(0); let last = crate::thread_tree::git(&["log", "-1", "--format=%s"], &p).map(|o| o.trim().to_string()).unwrap_or_default();
                json!({ "thread": thread, "root": p.to_string_lossy(), "files": files, "last": last }) }).collect(); Ok(json!({ "ok": true, "trees": trees })) }
        "widgets" | "list" => { let ws = all_widgets(root); Ok(json!({ "ok": true, "widgets": ws, "packages": packages(root).iter().map(|p| json!({ "name": p["name"], "version": p["version"], "widgets": p["widgets"].as_array().map(|l| l.len()).unwrap_or(0) })).collect::<Vec<_>>(), "proposals": proposals(root).iter().map(|p| json!({ "name": p["name"], "version": p["version"], "widgets": p["widgets"].as_array().map(|l| l.len()).unwrap_or(0) })).collect::<Vec<_>>(), "renderers": RENDERERS })) }
        "widget_save" => { let w = a["widget"].clone(); validate_widget(&w, srcs)?; let id = s(&w["id"]);
            if base_widgets().iter().any(|b| s(&b["id"]) == id) { return Err(format!("`{id}` is a base widget: base widgets are read-only; save yours under another id")); }
            if w.get("base").is_some() { return Err("a widget cannot say it is a base widget".into()); }
            let mut list = custom_widgets(root); let had = list.iter().any(|x| s(&x["id"]) == id); list.retain(|x| s(&x["id"]) != id); list.push(w.clone()); write_json(&dir(root).join("widgets.json"), &json!({ "widgets": list }))?;
            Ok(json!({ "ok": true, "saved": id, "replaced": had })) }
        "widget_remove" => { let id = s(&a["id"]); let mut list = custom_widgets(root); let n = list.len(); list.retain(|x| s(&x["id"]) != id); if list.len() == n { return Err(format!("`{id}` is not one of your widgets (base and package widgets are removed with their package, or not at all)")); } write_json(&dir(root).join("widgets.json"), &json!({ "widgets": list }))?; Ok(json!({ "ok": true, "removed": id })) }
        "layout" | "get" => { let screen = { let t = s(&a["screen"]); if t.is_empty() { "home".to_string() } else { t } }; screen_ok(&screen)?; let mut l = current_layout(root, &screen); l["ok"] = json!(true); Ok(l) }
        "layout_set" | "set" => { let screen = { let t = s(&a["screen"]); if t.is_empty() { "home".to_string() } else { t } }; let l = set_layout(root, &screen, &a["grid"], by, &s(&a["note"]))?; Ok(json!({ "ok": true, "layout": l })) }
        "layout_place" => { // put a declared widget on a screen at the first free spot (what "add to the board" does)
            let screen = { let t = s(&a["screen"]); if t.is_empty() { "home".to_string() } else { t } }; let id = s(&a["widget"]); let cur = current_layout(root, &screen); let mut grid = cur["grid"].as_array().cloned().unwrap_or_default(); let cells = cells_of(&Value::Array(grid.clone()))?;
            let (w, h) = (a["w"].as_u64().unwrap_or(6).clamp(1, 12) as u32, a["h"].as_u64().unwrap_or(4).clamp(1, 12) as u32); let (x, y) = free_spot(&cells, w, h); grid.push(json!({ "widget": id, "x": x, "y": y, "w": w, "h": h }));
            let l = set_layout(root, &screen, &Value::Array(grid), by, &format!("added {id}"))?; Ok(json!({ "ok": true, "layout": l })) }
        "layout_undo" | "undo" => { let screen = { let t = s(&a["screen"]); if t.is_empty() { "home".to_string() } else { t } }; screen_ok(&screen)?; let mine: Vec<Value> = versions(root).into_iter().filter(|v| s(&v["screen"]) == screen).collect();
            if mine.is_empty() { return Err("nothing to undo: this screen is still the default".into()); }
            let prev = if mine.len() >= 2 { mine[mine.len() - 2]["grid"].clone() } else { default_grid(&screen) }; let known = ids_of(&all_widgets(root)); let kept: Vec<Value> = prev.as_array().cloned().unwrap_or_default().into_iter().filter(|c| known.contains(&s(&c["widget"]))).collect();
            let l = set_layout(root, &screen, &Value::Array(kept), by, &format!("undo of version {}", mine.len()))?; Ok(json!({ "ok": true, "layout": l })) }
        "layout_history" | "history" => { let screen = { let t = s(&a["screen"]); if t.is_empty() { "home".to_string() } else { t } }; Ok(json!({ "ok": true, "screen": screen, "versions": versions(root).iter().filter(|v| s(&v["screen"]) == screen).map(|v| json!({ "n": v["n"], "by": v["by"], "note": v["note"], "ts": v["ts"], "widgets": v["grid"].as_array().map(|g| g.len()).unwrap_or(0) })).collect::<Vec<_>>() })) }
        "views" => { let mut names: Vec<String> = versions(root).iter().map(|v| s(&v["screen"])).filter(|t| t.starts_with("view:")).collect(); names.sort(); names.dedup(); Ok(json!({ "ok": true, "views": names.iter().map(|n| json!({ "screen": n, "name": n.trim_start_matches("view:"), "widgets": current_layout(root, n)["grid"].as_array().map(|g| g.len()).unwrap_or(0) })).collect::<Vec<_>>() })) }
        "view_save" => { let name = s(&a["name"]); if !slug_ok(&name) { return Err("a view has a name of a-z, 0-9, - or _".into()); } let grid = if a["grid"].is_array() { a["grid"].clone() } else { current_layout(root, &{ let t = s(&a["from"]); if t.is_empty() { "home".to_string() } else { t } })["grid"].clone() };
            let l = set_layout(root, &format!("view:{name}"), &grid, by, &s(&a["note"]))?; Ok(json!({ "ok": true, "layout": l })) }
        "slots" => { let ov = slot_overrides(root); Ok(json!({ "ok": true, "slots": SLOTS().iter().map(|(k, about)| json!({ "slot": k, "about": about, "state": if ov.get(*k).is_some() { "replaced" } else { "built-in" }, "widget": ov.get(*k).cloned().unwrap_or(Value::Null), "from": slot_source(root, k) })).collect::<Vec<_>>() })) }
        "slot_set" => { let slot = s(&a["slot"]); let wid = s(&a["widget"]); if !SLOTS().iter().any(|x| x.0 == slot) { return Err(format!("no slot `{slot}`: the slots are {}", SLOTS().iter().map(|x| x.0).collect::<Vec<_>>().join(", "))); }
            if !ids_of(&all_widgets(root)).contains(&wid) { return Err(format!("no widget `{wid}`: declare it first (antiloki_xray_widget_save)")); }
            let mut m = own_slots(root); m.insert(slot.clone(), json!(wid)); write_json(&dir(root).join("slots.json"), &Value::Object(m))?; Ok(json!({ "ok": true, "slot": slot, "widget": wid, "note": "the built-in is still there underneath: restore it with op `restore`" })) }
        "slot_reset" => { let slot = s(&a["slot"]); let mut m = own_slots(root); if m.remove(&slot).is_none() { return Err(format!("`{slot}` is not replaced: it is the built-in already")); } write_json(&dir(root).join("slots.json"), &Value::Object(m))?; Ok(json!({ "ok": true, "slot": slot, "restored": true })) }
        "shape_export" => { let theme = s(&a["theme"]); let sh = shape_of(root); let text = shape_to_yaml(&sh)?; let d = theme_dir(&theme, true).ok_or_else(|| format!("no theme `{theme}` of yours to write into: only a theme in antiloki's own store can carry a shape (install one, or name one that is there)"))?; std::fs::write(d.join("xray.yaml"), &text).map_err(|e| e.to_string())?; Ok(json!({ "ok": true, "theme": theme, "bytes": text.len(), "path": d.join("xray.yaml").to_string_lossy() })) }
        "shape_import" => { if agent && a["yaml"].is_string() { return Err("an agent imports a shape only from a theme the person chose: pass `theme`".into()); } let text = if a["yaml"].is_string() { s(&a["yaml"]) } else { let theme = s(&a["theme"]); let d = theme_dir(&theme, false).ok_or_else(|| format!("no theme `{theme}`"))?; std::fs::read_to_string(d.join("xray.yaml")).map_err(|_| format!("the theme `{theme}` has no xray.yaml"))? };
            let sh = shape_from_yaml(&text, srcs)?; let n = apply_shape(root, srcs, &sh, by)?; Ok(json!({ "ok": true, "imported": n })) }
        "shape" => Ok(json!({ "ok": true, "yaml": shape_to_yaml(&shape_of(root))?, "from_theme": theme_shape(root).is_some() })),
        "propose" => { let p = a["package"].clone(); validate_package(&p, srcs)?; write_json(&dir(root).join("proposals").join(format!("{}.json", s(&p["name"]))), &p)?; Ok(json!({ "ok": true, "proposed": s(&p["name"]), "note": "the person reads it in the X-ray store and installs it; you cannot" })) }
        "install" => { if agent { return Err("only the person installs a package: propose it with op `propose` and the person installs it from the X-ray store".into()); }
            let p = if a["package"].is_object() { a["package"].clone() } else { let n = s(&a["proposal"]); read_json(&dir(root).join("proposals").join(format!("{n}.json"))).ok_or_else(|| format!("no proposal called `{n}`"))? };
            validate_package(&p, srcs)?; let name = s(&p["name"]); write_json(&dir(root).join("packages").join(format!("{name}.json")), &p)?; let _ = std::fs::remove_file(dir(root).join("proposals").join(format!("{name}.json")));
            if let Some(ls) = p["layouts"].as_array() { for l in ls { let screen = format!("view:{name}-{}", { let t = s(&l["screen"]); if t.is_empty() { "view".to_string() } else { t.replace(':', "-") } }); let grid: Vec<Value> = l["grid"].as_array().cloned().unwrap_or_default().into_iter().map(|mut c| { c["widget"] = json!(format!("{name}.{}", s(&c["widget"]))); c }).collect(); let _ = set_layout(root, &screen, &Value::Array(grid), "person", &format!("from the package {name}")); } }
            Ok(json!({ "ok": true, "installed": name })) }
        "remove" => { if agent { return Err("only the person removes a package".into()); } let name = s(&a["name"]); let f = dir(root).join("packages").join(format!("{name}.json")); if !f.exists() { return Err(format!("no package `{name}` is installed")); } std::fs::remove_file(f).map_err(|e| e.to_string())?; Ok(json!({ "ok": true, "removed": name })) }
        "discard" => { let name = s(&a["name"]); let _ = std::fs::remove_file(dir(root).join("proposals").join(format!("{name}.json"))); Ok(json!({ "ok": true, "discarded": name })) }
        other => Err(format!("unknown operation `{other}`")),
    }
}

/// The widget an `id` or an inline declaration names, validated. Pure over the folder.
pub fn widget_for(root: &str, srcs: &[SrcInfo], a: &Value) -> Result<Value, String> {
    if a["widget"].is_object() { validate_widget(&a["widget"], srcs)?; return Ok(a["widget"].clone()); }
    let id = s(&a["id"]); all_widgets(root).into_iter().find(|w| s(&w["id"]) == id).ok_or_else(|| format!("no widget `{id}`"))
}
/// Draw a widget: its source is read, the answer shaped for its renderer. Blocking (reads the database).
pub fn draw(db: &rusqlite::Connection, root: &str, w: &Value) -> Result<Value, String> {
    let generated = "x-ray"; let args = if w["args"].is_object() { w["args"].clone() } else { json!({}) }; let source = s(&w["source"]);
    if source == "slot" { let data = shape("component", &w["options"], "", false)?; return Ok(json!({ "ok": true, "id": w["id"], "title": w["title"], "view": "component", "summary": "a built-in part of X-ray", "data": data })); }
    let (content, is_json, summary) = if LOCAL.iter().any(|l| l.0 == source) { let (c, j) = local_read(root, &source, &args)?; (c, j, source.replace('_', " ")) } else { let out = antiloki_rs::cockpit__helpers_context_catalog::run(db, root, &source, &args, &generated)?; (strip_stamp(&out.content), out.json, out.summary) };
    let data = shape(&s(&w["view"]), &w["options"], &content, is_json)?;
    Ok(json!({ "ok": true, "id": w["id"], "title": w["title"], "view": w["view"], "summary": summary, "data": data }))
}

/// EVERYTHING THE X-RAY SCREEN DRAWS, IN ONE READING (the operator 2026-11-19: "one screen, pretty animated deterministic visual widgets"): the team runs and what they made (read from the workspace the teams live in), and the files and commits of the codebase being looked at (the workspace folder or a team's tree). Deterministic: the same folders give the same answer.
/// What the code analysis knows of a space, as data (nothing drawn here), TRIMMED to what the screen draws: on a big workspace the whole reports are close to a megabyte and took 5 to 10 seconds. Empty parts when the space was never indexed. Blocking.
pub fn health_of(db: &rusqlite::Connection) -> Value {
    let unwrap = |r: Result<Value, String>| -> Value { r.ok().map(|v| v.get("result").cloned().unwrap_or(v)).unwrap_or(Value::Null) };
    let take = |v: &Value, k: &str, n: usize| -> Value { Value::Array(v.get(k).and_then(|x| x.as_array()).map(|a| a.iter().take(n).cloned().collect()).unwrap_or_default()) };
    let rep = unwrap(antiloki_rs::constitution_constitution_report::constitutionReport(&json!({}), db));
    let report = if rep.is_null() { Value::Null } else { let sm = &rep["summary"]; json!({ "summary": { "health": sm["health"], "compliance": sm["compliance"], "violations": sm["violations"], "axes_avg": sm["axes_avg"], "knowledge": { "hotspots": take(&sm["knowledge"], "hotspots", 6) } },
        "by_law": Value::Array(rep["by_law"].as_array().map(|a| a.iter().take(8).map(|l| json!({ "law_key": l["law_key"], "title": l["title"], "category": l["category"], "n": l["n"], "mass": l["mass"] })).collect()).unwrap_or_default()), "by_category": take(&rep, "by_category", 10),
        "worst": Value::Array(rep["worst"].as_array().map(|a| a.iter().take(6).map(|w| json!({ "path": w["path"], "score": w["score"] })).collect()).unwrap_or_default()) }) };
    let sm = unwrap(antiloki_rs::codebase_atlas_smells::atlasSmells(&json!({}), db));
    let smells = if sm.is_null() { Value::Null } else { json!({ "counts": sm["counts"], "file_count": sm["file_count"], "smells": Value::Array(sm["smells"].as_array().map(|a| a.iter().filter(|x| x["kind"] == json!("god-file")).take(4).map(|x| json!({ "kind": x["kind"], "severity": x["severity"], "nodes": x["nodes"].as_array().map(|n| n.iter().take(1).cloned().collect::<Vec<_>>()).unwrap_or_default(), "detail": x["detail"] })).collect()).unwrap_or_default()) }) };
    let au = unwrap(antiloki_rs::codebase_code_authors::codeAuthors(&json!({ "top_authors": 5, "files_per_author": 1 }), db));
    let authors = if au.is_null() { Value::Null } else { json!({ "commit_count": au["commit_count"], "authors": Value::Array(au["authors"].as_array().map(|a| a.iter().take(5).map(|x| json!({ "id": x["id"], "name": x["name"], "commits": x["commits"], "files": x["files"], "solo": x["solo"], "first": x["first"], "last": x["last"] })).collect()).unwrap_or_default()), "bus_factor": json!({ "total_files": au["bus_factor"]["total_files"], "solo_files": au["bus_factor"]["solo_files"] }) }) };
    json!({ "report": report, "smells": smells, "authors": authors })
}
/// The analysis of a space, kept for a minute: the screen polls every few seconds and the analysis of a big workspace takes seconds. Returns what is kept (null before the first reading) and refreshes it in the background, one reading at a time per space. Does not block.
pub fn health_cached(root: &str, conn: std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>) -> Value {
    use std::sync::{Mutex, OnceLock}; static KEPT: OnceLock<Mutex<std::collections::HashMap<String, (std::time::Instant, Value, bool)>>> = OnceLock::new();
    let kept = KEPT.get_or_init(Default::default); let (hit, old) = { let g = kept.lock().unwrap_or_else(|p| p.into_inner()); match g.get(root) { Some((at, v, busy)) => (v.clone(), at.elapsed().as_secs() >= 60 && !busy), None => (Value::Null, true) } };
    if old { { let mut g = kept.lock().unwrap_or_else(|p| p.into_inner()); let e = g.entry(root.to_string()).or_insert((std::time::Instant::now(), Value::Null, false)); if e.2 { return hit; } e.2 = true; }
        let key = root.to_string(); std::thread::spawn(move || { let v = { let g = conn.lock().unwrap_or_else(|p| p.into_inner()); health_of(&g) }; KEPT.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).insert(key, (std::time::Instant::now(), v, false)); }); }
    hit
}
pub fn dash(root_files: &str, root_runs: &str, health: Value) -> Value {
    let rows = |key: &str, root: &str, args: Value| -> Value { local_read(root, key, &args).ok().and_then(|(c, _)| serde_json::from_str::<Value>(&c).ok()).unwrap_or(json!([])) };
    let tree = tree_files_of(root_files);
    let commits: Vec<Value> = crate::thread_tree::git(&["log", "-n", "40", "--format=%h%x1f%ct%x1f%an%x1f%s"], Path::new(root_files)).map(|o| o.lines().filter_map(|l| { let p: Vec<&str> = l.split('\u{1f}').collect(); (p.len() == 4).then(|| json!({ "sha": p[0], "ts": p[1].parse::<u64>().unwrap_or(0) * 1000, "author": p[2], "subject": p[3] })) }).collect()).unwrap_or_default();
    json!({ "ok": true, "now": now_ms() as u64, "runs": rows("team_runs", root_runs, json!({ "limit": 200 })), "outputs": rows("team_outputs", root_runs, json!({ "limit": 500 })), "stats": rows("team_stats", root_runs, json!({})), "kinds": rows("file_types", root_files, json!({})), "files": tree, "commits": commits, "health": health })
}
/// The files of a space (git's own list, else a walk), with size and kind. Blocking.
pub fn tree_files_of(root: &str) -> Value {
    let r = Path::new(root); let mut names: Vec<String> = crate::thread_tree::git(&["ls-files"], r).map(|o| o.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default();
    if names.is_empty() { let mut v = vec![]; walk_files(r, r, 0, &["node_modules", "target", "dist", "build", "venv", ".venv", "__pycache__"], &mut v); names = v.into_iter().map(|x| x.0).collect(); }
    names.retain(|n| !crate::team_run::NOT_WORK.contains(&n.as_str()) && !n.starts_with(".antiloki/")); names.sort(); names.truncate(500);
    Value::Array(names.iter().map(|n| json!({ "path": n, "kb": ((std::fs::metadata(r.join(n)).map(|m| m.len()).unwrap_or(0) as f64) / 1024.0 * 10.0).round() / 10.0, "kind": kind_of_file(n) })).collect())
}
// ── the door: HTTP for the screen, the tools for the agents ───────────────────────────────────────
use crate::cowork_xray::{registry, resolve};
use crate::dispatch::AppState;
use std::sync::Arc;

/// One call, by the person (the screen) or an agent (the MCP): the store operations, and the two that need the database (`data` for a widget by id or declaration, `preview` for a declaration not yet saved).
pub async fn call(state: &Arc<AppState>, space: &str, op: &str, a: &Value, by: &str) -> Result<Value, String> {
    let sp = resolve(state, space)?; let root = sp.root.clone(); let srcs = catalog();
    if op == "data" || op == "preview" {
        let w = { let (r, s2, a2) = (root.clone(), srcs.clone(), a.clone()); tokio::task::spawn_blocking(move || widget_for(&r, &s2, &a2)).await.map_err(|e| e.to_string())?? };
        let (conn, r2) = (sp.conn.clone(), root.clone());
        return tokio::task::spawn_blocking(move || { let g = conn.lock().unwrap_or_else(|p| p.into_inner()); draw(&g, &r2, &w) }).await.map_err(|e| e.to_string())?;
    }
    if op == "dash" { let runs_root = { let t = s(&a["runs_space"]); if t.is_empty() { registry().into_iter().find(|x| x.2).map(|x| x.1).unwrap_or_else(|| root.clone()) } else { resolve(state, &t)?.root } }; let (rf, conn) = (root.clone(), sp.conn.clone()); return tokio::task::spawn_blocking(move || { let health = health_cached(&rf, conn); dash(&rf, &runs_root, health) }).await.map_err(|e| e.to_string()); }
    let (op2, a2, by2) = (op.to_string(), a.clone(), by.to_string());
    tokio::task::spawn_blocking(move || run_op(&root, &srcs, &op2, &a2, &by2)).await.map_err(|e| e.to_string())?
}
/// `POST /api/v1/xray/v2 {op, space?, ...}`
pub async fn http(state: Arc<AppState>, body: axum::body::Bytes) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse, Json};
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let out = call(&state, &s(&b["space"]), &s(&b["op"]), &b, "person").await;
    match out { Ok(v) => (StatusCode::OK, Json(v)).into_response(), Err(e) => (StatusCode::OK, Json(json!({ "ok": false, "error": e }))).into_response() }
}
/// The tools. Each is a fixed set of operations; an agent writes declarations and layouts, never a package install.
pub async fn run_tool(state: &Arc<AppState>, tool: &str, args: &Value) -> Result<String, String> {
    let space = s(&args["space"]); let pick = |allowed: &[&str], dflt: &str| -> Result<String, String> { let op = { let t = s(&args["op"]); if t.is_empty() { dflt.to_string() } else { t } }; if allowed.contains(&op.as_str()) { Ok(op) } else { Err(format!("`{op}` is not an operation of {tool}: {}", allowed.join(", "))) } };
    let (op, a) = match tool {
        "antiloki_xray_widgets" => (pick(&["list", "sources"], "list")?, args.clone()),
        "antiloki_xray_widget_save" => (match pick(&["save", "remove"], "save")?.as_str() { "remove" => "widget_remove".to_string(), _ => "widget_save".to_string() }, args.clone()),
        "antiloki_xray_layout" => (match pick(&["get", "set", "place", "undo", "history", "views", "view_save"], "get")?.as_str() { "get" => "layout", "set" => "layout_set", "place" => "layout_place", "undo" => "layout_undo", "history" => "layout_history", "views" => "views", _ => "view_save" }.to_string(), args.clone()),
        "antiloki_xray_install" => (pick(&["propose", "install", "remove", "discard"], "propose")?, args.clone()),
        "antiloki_xray_slots" => (match pick(&["list", "replace", "restore", "export", "import", "shape"], "list")?.as_str() { "list" => "slots", "replace" => "slot_set", "restore" => "slot_reset", "export" => "shape_export", "import" => "shape_import", _ => "shape" }.to_string(), args.clone()),
        "antiloki_xray_preview" => (if args["id"].is_string() { "data".to_string() } else { "preview".to_string() }, args.clone()),
        other => return Err(format!("unknown tool {other}")),
    };
    let v = call(state, &space, &op, &a, "agent").await?; Ok(serde_json::to_string_pretty(&v).unwrap_or_default().chars().take(40_000).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> String { let p = std::env::temp_dir().join(format!("antiloki-xray2-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); p.to_string_lossy().to_string() }
    fn w(id: &str, source: &str, view: &str) -> Value { json!({ "id": id, "title": id, "source": source, "view": view, "args": {}, "options": {} }) }

    #[test]
    fn every_base_widget_is_a_declaration_the_validator_accepts() {
        let srcs = catalog(); assert!(srcs.len() > 20);
        for b in base_widgets() { let mut x = b.clone(); x.as_object_mut().unwrap().remove("base"); validate_widget(&x, &srcs).unwrap_or_else(|e| panic!("{}: {e}", s(&b["id"]))); }
        let ids = ids_of(&base_widgets()); assert_eq!(ids.len(), ids.iter().collect::<std::collections::HashSet<_>>().len(), "unique ids");
        assert!(validate_layout(&default_grid("home"), &ids).is_ok(), "the default grid fits");
    }
    #[test]
    fn the_validator_refuses_ai_sources_unknown_things_wrong_arguments_and_code() {
        let srcs = catalog(); assert!(validate_widget(&w("a", "laws", "list"), &srcs).is_ok());
        let ai = antiloki_rs::cockpit__helpers_context_catalog::CATALOG.iter().find(|c| c.ai).expect("an AI source exists").key; assert!(validate_widget(&w("a", ai, "text"), &srcs).unwrap_err().contains("costs an AI call"));
        assert!(validate_widget(&w("a", "nope", "list"), &srcs).unwrap_err().contains("unknown source")); assert!(validate_widget(&w("a", "laws", "iframe"), &srcs).unwrap_err().contains("unknown renderer"));
        let mut bad = w("a", "laws", "list"); bad["args"] = json!({ "zzz": 1 }); assert!(validate_widget(&bad, &srcs).unwrap_err().contains("not an argument"));
        let mut typed = w("a", "file", "text"); typed["args"] = json!({ "path": { "x": 1 } }); assert!(validate_widget(&typed, &srcs).unwrap_err().contains("string, a number"));
        let mut code = w("a", "laws", "list"); code["html"] = json!("<script>1</script>"); assert!(validate_widget(&code, &srcs).unwrap_err().contains("never code")); let mut code2 = w("a", "laws", "list"); code2["script"] = json!("alert(1)"); assert!(validate_widget(&code2, &srcs).is_err());
        assert!(validate_widget(&w("Bad Id", "laws", "list"), &srcs).is_err()); let mut opt = w("a", "laws", "list"); opt["options"] = json!({ "onclick": "x" }); assert!(validate_widget(&opt, &srcs).is_err());
    }
    #[test]
    fn a_layout_stays_inside_twelve_columns_never_overlaps_and_names_declared_widgets() {
        let known = vec!["a".to_string(), "b".to_string()];
        assert!(validate_layout(&json!([{ "widget": "a", "x": 0, "y": 0, "w": 6, "h": 3 }, { "widget": "b", "x": 6, "y": 0, "w": 6, "h": 3 }]), &known).is_ok());
        assert!(validate_layout(&json!([{ "widget": "a", "x": 0, "y": 0, "w": 6, "h": 3 }, { "widget": "b", "x": 5, "y": 2, "w": 6, "h": 3 }]), &known).unwrap_err().contains("overlaps"));
        assert!(validate_layout(&json!([{ "widget": "a", "x": 8, "y": 0, "w": 6, "h": 3 }]), &known).unwrap_err().contains("past the right edge")); assert!(validate_layout(&json!([{ "widget": "zz", "x": 0, "y": 0, "w": 2, "h": 2 }]), &known).unwrap_err().contains("not declared"));
        assert!(validate_layout(&json!([{ "widget": "a", "x": 0, "y": 0, "w": 2, "h": 2 }, { "widget": "a", "x": 4, "y": 0, "w": 2, "h": 2 }]), &known).unwrap_err().contains("twice"));
        let cells = cells_of(&json!([{ "widget": "a", "x": 0, "y": 0, "w": 6, "h": 3 }])).unwrap(); assert_eq!(free_spot(&cells, 6, 3), (6, 0)); assert_eq!(free_spot(&cells, 12, 2), (0, 3));
    }
    #[test]
    fn every_part_of_xray_is_a_slot_replaceable_and_restorable_and_the_shape_round_trips_as_yaml() {
        let root = tmp("slots"); let srcs = catalog(); let slots = run_op(&root, &srcs, "slots", &json!({}), "agent").unwrap(); assert!(slots["slots"].as_array().unwrap().len() >= 19); assert!(slots["slots"].as_array().unwrap().iter().all(|x| x["state"] == json!("built-in")), "today's X-ray is the default");
        assert!(run_op(&root, &srcs, "slot_set", &json!({ "slot": "overview", "widget": "nope" }), "agent").is_err()); assert!(run_op(&root, &srcs, "slot_set", &json!({ "slot": "zzz", "widget": "laws" }), "agent").is_err());
        run_op(&root, &srcs, "widget_save", &json!({ "widget": w("mine", "digest", "text") }), "agent").unwrap(); run_op(&root, &srcs, "slot_set", &json!({ "slot": "overview", "widget": "mine" }), "agent").unwrap();
        run_op(&root, &srcs, "slot_set", &json!({ "slot": "sec-runs", "widget": "mine" }), "agent").unwrap(); assert!(run_op(&root, &srcs, "slots", &json!({}), "agent").unwrap()["slots"].as_array().unwrap().iter().any(|x| x["slot"] == json!("sec-runs") && x["state"] == json!("replaced")), "a section of the screen is a slot"); run_op(&root, &srcs, "slot_reset", &json!({ "slot": "sec-runs" }), "agent").unwrap();
        let st = run_op(&root, &srcs, "slots", &json!({}), "agent").unwrap(); let ov = st["slots"].as_array().unwrap().iter().find(|x| x["slot"] == json!("overview")).unwrap().clone(); assert_eq!(ov["state"], json!("replaced")); assert_eq!(ov["widget"], json!("mine"));
        run_op(&root, &srcs, "layout_set", &json!({ "screen": "team-a", "grid": [{ "widget": "mine", "x": 0, "y": 0, "w": 6, "h": 3 }] }), "agent").unwrap();
        let y = run_op(&root, &srcs, "shape", &json!({}), "agent").unwrap()["yaml"].as_str().unwrap().to_string(); assert!(y.contains("overview: mine") && y.contains("team-a"), "{y}");
        let back = shape_from_yaml(&y, &srcs).unwrap(); assert_eq!(back["slots"]["overview"], json!("mine")); assert_eq!(back["widgets"][0]["id"], json!("mine"));
        let other = tmp("slots2"); assert_eq!(run_op(&other, &srcs, "shape_import", &json!({ "yaml": y }), "person").unwrap()["imported"], json!(3)); assert_eq!(run_op(&other, &srcs, "slots", &json!({}), "agent").unwrap()["slots"][0]["state"].as_str().is_some(), true);
        assert!(run_op(&other, &srcs, "shape_import", &json!({ "yaml": y }), "agent").unwrap_err().contains("only from a theme"), "an agent does not push raw YAML");
        run_op(&root, &srcs, "slot_reset", &json!({ "slot": "overview" }), "agent").unwrap(); assert!(run_op(&root, &srcs, "slot_reset", &json!({ "slot": "overview" }), "agent").is_err(), "restoring twice says it is already the built-in");
        for bad in ["xray:\n  code: 1", "xray:\n  slots:\n    overview: ghost", "xray:\n  widgets:\n    - {id: x, title: x, source: describe, view: text}", "nothing: here", "xray:\n  screens:\n    home:\n      - {widget: laws, x: 9, y: 0, w: 6, h: 2}"] { assert!(shape_from_yaml(bad, &srcs).is_err(), "{bad}"); }
        let _ = std::fs::remove_dir_all(&root); let _ = std::fs::remove_dir_all(&other);
    }
    #[test]
    fn the_dash_reads_runs_from_one_folder_and_files_and_commits_from_another() {
        let runs = tmp("dashr"); let files = tmp("dashf"); let r = Path::new(&runs); std::fs::create_dir_all(r.join(".antiloki/team/run-1")).unwrap();
        std::fs::write(r.join(".antiloki/team/run-1/state.json"), json!({ "id": "run-1", "team": "T", "ask": "a", "state": { "status": { "status": "done" }, "spent_usd": 0.5, "started_ms": 9 } }).to_string()).unwrap(); std::fs::write(Path::new(&files).join("a.md"), "x").unwrap(); std::fs::write(Path::new(&files).join("b.html"), "<p>").unwrap();
        let d = dash(&files, &runs, Value::Null); assert_eq!(d["runs"].as_array().unwrap().len(), 1); assert_eq!(d["stats"]["spent_usd"], json!(0.5)); assert_eq!(d["files"].as_array().unwrap().len(), 2); assert_eq!(d["kinds"]["md"], json!(1)); assert!(d["commits"].as_array().unwrap().is_empty(), "no repo, no commits");
        let (mut x, mut y) = (dash(&files, &runs, Value::Null), dash(&files, &runs, Value::Null)); x["now"] = json!(0); y["now"] = json!(0); assert_eq!(x, y, "the same folders give the same reading (apart from the clock)"); let _ = std::fs::remove_dir_all(&runs); let _ = std::fs::remove_dir_all(&files);
    }
    #[test]
    fn the_components_x_ray_has_today_are_base_widgets_drawn_by_name_and_nothing_else_may_pose_as_one() {
        let srcs = catalog(); let all = base_widgets(); for (k, _) in COMPONENTS { let w = all.iter().find(|x| s(&x["id"]) == format!("x-{k}")).unwrap_or_else(|| panic!("{k}")); assert_eq!(w["view"], json!("component")); assert!(SLOTS().iter().any(|x| x.0 == k), "{k} is a slot"); }
        let mut c = json!({ "id": "mine", "title": "mine", "source": "slot", "view": "component", "options": { "name": "overview" } }); assert!(validate_widget(&c, &srcs).is_ok()); c["options"] = json!({ "name": "ghost" }); assert!(validate_widget(&c, &srcs).is_err()); c["options"] = json!({ "name": "overview" }); c["view"] = json!("text"); assert!(validate_widget(&c, &srcs).is_err());
        assert_eq!(shape("component", &json!({ "name": "rules" }), "", false).unwrap()["name"], json!("rules")); assert!(shape("component", &json!({ "name": "../x" }), "", false).is_err());
    }
    #[test]
    fn a_workspace_without_code_still_has_what_its_teams_made_and_the_kinds_of_file_it_holds() {
        let root = tmp("nocode"); let r = Path::new(&root); std::fs::create_dir_all(r.join(".antiloki/team/run-1/artifacts")).unwrap(); std::fs::create_dir_all(r.join(".antiloki/team/run-1/media/video")).unwrap();
        std::fs::write(r.join(".antiloki/team/run-1/state.json"), json!({ "id": "run-1", "team": "Video", "ask": "make a promo", "state": { "status": { "status": "done" }, "spent_usd": 0.126, "started_ms": 5 } }).to_string()).unwrap();
        std::fs::write(r.join(".antiloki/team/run-1/artifacts/report.md"), "hello").unwrap(); std::fs::write(r.join(".antiloki/team/run-1/media/video/index.html"), "<p>x</p>").unwrap(); std::fs::write(r.join("AGENTS.md"), "a").unwrap(); std::fs::write(r.join("plan.md"), "b").unwrap(); std::fs::write(r.join("x.json"), "{}").unwrap();
        let (c, j) = local_read(&root, "team_runs", &json!({})).unwrap(); assert!(j); let runs: Value = serde_json::from_str(&c).unwrap(); assert_eq!(runs[0]["team"], json!("Video")); assert_eq!(runs[0]["status"], json!("done")); assert_eq!(runs[0]["spent_usd"], json!(0.13));
        let outs: Value = serde_json::from_str(&local_read(&root, "team_outputs", &json!({})).unwrap().0).unwrap(); assert_eq!(outs.as_array().unwrap().len(), 2); assert!(outs.as_array().unwrap().iter().any(|o| o["kind"] == json!("page") && o["from"] == json!("media")));
        let ft: Value = serde_json::from_str(&local_read(&root, "file_types", &json!({})).unwrap().0).unwrap(); assert_eq!(ft["md"], json!(2)); assert_eq!(ft["json"], json!(1), "a hidden folder (.antiloki) is not counted"); let docs: Value = serde_json::from_str(&local_read(&root, "docs", &json!({})).unwrap().0).unwrap(); assert_eq!(docs.as_array().unwrap().len(), 2);
        // they draw through the same renderers as any widget, and they validate like any declaration
        let srcs = catalog(); for b in base_widgets() { let mut x = b.clone(); x.as_object_mut().unwrap().remove("base"); validate_widget(&x, &srcs).unwrap(); }
        let stats: Value = serde_json::from_str(&local_read(&root, "team_stats", &json!({})).unwrap().0).unwrap(); assert_eq!(stats["runs"], json!(1)); assert_eq!(stats["done"], json!(1)); assert_eq!(stats["failed"], json!(0)); assert_eq!(stats["made"], json!(2)); assert_eq!(stats["videos"], json!(1)); assert_eq!(stats["documents"], json!(2)); assert_eq!(shape("number", &json!({ "path": "runs" }), &local_read(&root, "team_stats", &json!({})).unwrap().0, true).unwrap()["value"], json!(1.0));
        let media = shape("media", &json!({}), &local_read(&root, "team_outputs", &json!({})).unwrap().0, true).unwrap(); assert_eq!(media["items"].as_array().unwrap().len(), 1); assert_eq!(media["items"][0]["kind"], json!("page")); assert_eq!(media["items"][0]["file"], json!("kept/video/index.html"), "the media route names a kept file like this");
        assert!(shape("media", &json!({}), "[]", true).unwrap_err().contains("no video"));
        assert!(shape("table", &json!({}), &c, true).unwrap()["columns"].as_array().unwrap().iter().any(|x| x == "team")); assert_eq!(shape("bar", &json!({}), &local_read(&root, "file_types", &json!({})).unwrap().0, true).unwrap()["bars"][0]["label"], json!("md")); let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn a_team_has_its_own_xray_screen_that_starts_as_the_home_board_and_changes_alone() {
        let root = tmp("team"); let srcs = catalog(); assert_eq!(default_grid("team-video")[0]["widget"], json!("kpi-runs"), "a team's main screen is a dashboard"); assert!(default_grid("team-video-parts").as_array().unwrap().iter().any(|c| c["widget"] == json!("x-overview")), "the real X-ray components have their own screen"); assert_eq!(default_grid("home")[0]["widget"], json!("x-overview"), "the home board is today's X-ray first"); assert!(default_grid("team-video-data").as_array().unwrap().len() >= 10); for sc in ["team-video", "team-video-parts", "team-video-data", "home"] { assert!(validate_layout(&default_grid(sc), &ids_of(&base_widgets())).is_ok(), "{sc} fits"); }  assert_eq!(default_grid("view:x"), json!([]));
        run_op(&root, &srcs, "layout_set", &json!({ "screen": "team-video", "grid": [{ "widget": "laws", "x": 0, "y": 0, "w": 12, "h": 3 }] }), "agent").unwrap();
        assert_eq!(current_layout(&root, "team-video")["grid"].as_array().unwrap().len(), 1); assert_eq!(current_layout(&root, "home")["grid"], default_grid("home"), "the home board is untouched"); let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn the_clock_stamp_of_a_source_is_not_part_of_what_a_widget_draws() {
        assert_eq!(strip_stamp("<!-- antiloki context · kind=laws · generated=2026-11-19T02:35:31Z -->\n# Laws\nbody"), "# Laws\nbody"); assert_eq!(strip_stamp("plain\ntext"), "plain\ntext");
    }
    #[test]
    fn the_same_answer_always_draws_the_same_data_and_a_misfit_says_so() {
        let o = json!({}); let rows = r#"[{"name":"a","n":1},{"name":"b","n":2}]"#;
        let t = shape("table", &o, rows, true).unwrap(); assert_eq!(t["columns"], json!(["name", "n"])); assert_eq!(t["rows"][1], json!(["b", "2"])); assert_eq!(t, shape("table", &o, rows, true).unwrap());
        assert_eq!(shape("bar", &json!({ "value": "n" }), rows, true).unwrap()["bars"][1], json!({ "label": "b", "value": 2.0 })); assert_eq!(shape("number", &json!({ "path": "0.n" }), rows, true).unwrap()["value"], json!(1.0));
        assert_eq!(shape("list", &o, "x\n\ny\n", false).unwrap()["items"], json!(["x", "y"])); assert_eq!(shape("number", &o, "a\nb\nc", false).unwrap()["value"], json!(3.0));
        assert!(shape("table", &o, "plain words", false).unwrap_err().contains("not JSON")); assert!(shape("bar", &o, r#"{"a":"x"}"#, true).unwrap_err().contains("nothing to draw"));
        assert_eq!(shape("line", &o, "[1,3,2]", true).unwrap()["points"].as_array().unwrap().len(), 3); assert!(shape("line", &o, "[1]", true).is_err());
        assert_eq!(shape("graph", &json!({ "path": "edges" }), r#"{"edges":[{"from":"a","to":"b"}]}"#, true).unwrap()["edges"][0], json!({ "from": "a", "to": "b" }));
        assert_eq!(shape("diff", &o, "@@ -1 +1 @@\n-old\n+new\n same", false).unwrap()["lines"][2]["kind"], json!("add")); assert!(shape("iframe", &o, "x", false).is_err());
    }
    #[test]
    fn widgets_and_layouts_are_versioned_undoable_and_a_package_needs_the_person() {
        let root = tmp("e2e"); let srcs = catalog();
        // base widgets are read-only, a custom one is saved and listed, a bad one is refused
        assert!(run_op(&root, &srcs, "widget_save", &json!({ "widget": w("laws", "laws", "text") }), "agent").unwrap_err().contains("base widget"));
        run_op(&root, &srcs, "widget_save", &json!({ "widget": w("my-git", "git", "text") }), "agent").unwrap(); assert!(all_widgets(&root).iter().any(|x| s(&x["id"]) == "my-git"));
        assert!(run_op(&root, &srcs, "widget_save", &json!({ "widget": w("bad", "nope", "text") }), "agent").is_err());
        // the default layout, a move, an overlap refused, an undo
        assert_eq!(run_op(&root, &srcs, "layout", &json!({}), "agent").unwrap()["n"], json!(0));
        run_op(&root, &srcs, "layout_set", &json!({ "grid": [{ "widget": "laws", "x": 0, "y": 0, "w": 12, "h": 3 }] }), "agent").unwrap();
        let placed = run_op(&root, &srcs, "layout_place", &json!({ "widget": "my-git", "w": 6, "h": 3 }), "person").unwrap(); assert_eq!(placed["layout"]["n"], json!(2)); assert_eq!(placed["layout"]["grid"][1]["y"], json!(3));
        assert!(run_op(&root, &srcs, "layout_set", &json!({ "grid": [{ "widget": "laws", "x": 0, "y": 0, "w": 6, "h": 3 }, { "widget": "my-git", "x": 3, "y": 1, "w": 6, "h": 3 }] }), "agent").unwrap_err().contains("overlaps"));
        assert_eq!(run_op(&root, &srcs, "layout", &json!({}), "agent").unwrap()["n"], json!(2), "a refused change saves nothing");
        let undone = run_op(&root, &srcs, "layout_undo", &json!({}), "person").unwrap(); assert_eq!(undone["layout"]["grid"].as_array().unwrap().len(), 1); assert_eq!(undone["layout"]["n"], json!(3));
        let h = run_op(&root, &srcs, "layout_history", &json!({}), "agent").unwrap(); assert_eq!(h["versions"].as_array().unwrap().len(), 3); assert_eq!(h["versions"][1]["by"], json!("person"));
        // a named view
        run_op(&root, &srcs, "view_save", &json!({ "name": "morning" }), "agent").unwrap(); assert_eq!(run_op(&root, &srcs, "views", &json!({}), "agent").unwrap()["views"][0]["name"], json!("morning"));
        // a package: an agent proposes, only the person installs; its widgets are namespaced and leave with it
        let pkg = json!({ "name": "health", "version": "1", "widgets": [w("gitlog", "history", "list")], "layouts": [{ "screen": "main", "grid": [{ "widget": "gitlog", "x": 0, "y": 0, "w": 12, "h": 4 }] }] });
        assert!(run_op(&root, &srcs, "install", &json!({ "package": pkg }), "agent").unwrap_err().contains("only the person")); run_op(&root, &srcs, "propose", &json!({ "package": pkg }), "agent").unwrap();
        assert_eq!(run_op(&root, &srcs, "widgets", &json!({}), "agent").unwrap()["proposals"][0]["name"], json!("health"));
        run_op(&root, &srcs, "install", &json!({ "proposal": "health" }), "person").unwrap(); assert!(all_widgets(&root).iter().any(|x| s(&x["id"]) == "health.gitlog"));
        run_op(&root, &srcs, "layout_place", &json!({ "widget": "health.gitlog" }), "person").unwrap(); assert!(run_op(&root, &srcs, "remove", &json!({ "name": "health" }), "agent").is_err());
        run_op(&root, &srcs, "remove", &json!({ "name": "health" }), "person").unwrap(); assert!(!all_widgets(&root).iter().any(|x| s(&x["id"]) == "health.gitlog")); assert!(!current_layout(&root, "home")["grid"].to_string().contains("health.gitlog"), "a removed widget leaves the layouts");
        assert!(run_op(&root, &srcs, "install", &json!({ "package": { "name": "evil", "version": "1", "widgets": [{ "id": "x", "title": "x", "source": "laws", "view": "list", "script": "1" }] } }), "person").is_err(), "a package cannot do more than a hand-made widget");
        let _ = std::fs::remove_dir_all(&root);
    }
}
