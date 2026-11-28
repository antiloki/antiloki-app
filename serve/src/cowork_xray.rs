//! X-RAY FOR THE AGENTS, ACROSS SPACES (operator 2026-11-10: "list all features inside x-ray ... add all that to the mcp, so the mcp can read any of that. And also to edit, remove, activate, deactivate or add new
//! deterministic rules on quality. The MCP will be able to see the AST, file reviews, folder papers, architecture diagram, graph, anything we have on x-ray. Read, interpret and open a pane with exactly the part").
//!
//! Every tool here is about ONE space: the workspace named by `space` (its name or its root), else the one the screen has open. The data is that workspace's own database (`dispatch::workspace_conn`), so an agent in one
//! space can read, and a main agent can edit the rules of, another - and nothing is switched.
//!
//!   antiloki_spaces         the registered spaces, with what X-ray knows of each
//!   antiloki_xray           ANY X-ray feature by source key (the whole `antiloki_query` catalog, plus the Architecture / Design / History tabs), for any space
//!   antiloki_rules          the Quality tab's rules: list, show, add, edit, activate, deactivate, remove, reweight, except, unexcept (a main agent writes; a sub-agent only reads)
//!   antiloki_xray_rebuild   re-read a space's code into its atlas (the ↻ rebuild) and say what changed
//!   antiloki_xray_open      open a pane on the screen showing EXACTLY one part of X-ray (a file review, a folder paper, the system diagram, the import graph, the rules, the AST, ...) for a space
//!
//! Pure helpers are tested here; the handlers only find the connection and the clock.
use crate::dispatch::{workspace_conn, AppState};
use rusqlite::Connection;
use std::path::Path;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};

pub const TOOLS: [&str; 5] = ["antiloki_spaces", "antiloki_xray", "antiloki_rules", "antiloki_xray_rebuild", "antiloki_xray_open"];
/// The tools that change something: a sub-agent may not call them.
pub const WRITES: [&str; 2] = ["antiloki_rules", "antiloki_xray_rebuild"];

fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }

/// One registered space and the connection that answers for it.
pub struct Space { pub name: String, pub root: String, pub open: bool, pub conn: Arc<Mutex<Connection>> }

/// (name, root, open) for every registered space whose folder still exists. `open` = the one the serve's connections are bound to.
pub fn registry() -> Vec<(String, String, bool)> {
    let mut v: Vec<(String, String, bool)> = antiloki_rs::system_workspace_list::workspaceList(&json!({}), None).ok().and_then(|v| v["workspaces"].as_array().cloned()).unwrap_or_default().iter()
        .filter(|w| w["missing"] != json!(true))
        .map(|w| (s(&w["name"]), s(&w["root_path"]).trim_end_matches(['/', '\\']).to_string(), w["is_active"] == json!(1))).collect();
    // the trees of each space's threads are spaces too (named `<space> · <thread>`): the work of a team is the codebase, and X-ray reads it where it is
    let trees: Vec<(String, String, bool)> = v.iter().flat_map(|(name, root, _)| crate::thread_tree::trees_of(root).into_iter().map(move |(th, p)| (format!("{name} · {th}"), p.to_string_lossy().trim_end_matches(['/', '\\']).to_string(), false))).collect();
    v.extend(trees); v
}

/// Which registered space `want` names: its name (any case) or its root; empty means the open one. Pure over the list.
pub fn pick_space(list: &[(String, String, bool)], want: &str) -> Result<(String, String, bool), String> {
    let want = want.trim().trim_end_matches(['/', '\\']);
    let hit = if want.is_empty() { list.iter().find(|x| x.2) } else { list.iter().find(|x| x.0.eq_ignore_ascii_case(want) || x.1 == want) };
    hit.cloned().ok_or_else(|| if want.is_empty() { "no space is open: pass `space` (see antiloki_spaces)".to_string() } else { format!("no such space `{want}`; the spaces are: {}", list.iter().map(|x| x.0.as_str()).collect::<Vec<_>>().join(", ")) })
}

pub fn resolve(state: &AppState, want: &str) -> Result<Space, String> {
    // A RUN'S OWN FOLDER IS A SPACE TOO (2026-11-21, the Editor view: the agents' files live in the run's folder until a merge puts them in the thread's tree, and X-ray reads them where they are): a folder antiloki cut for a thread or a run inside its trees folder
    let reg = registry(); let (name, root, open) = match pick_space(&reg, want) { Ok(x) => x, Err(e) => { let w = want.trim().trim_end_matches(['/', '\\']); if w.starts_with('/') || w.get(1..3) == Some(":\\") || w.get(1..3) == Some(":/") { if crate::thread_tree::is_tree_dir(w) { (Path::new(w).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "run".into()), w.to_string(), false) } else { return Err(e); } } else { return Err(e); } } };
    let conn = if open { state.heal_if_gone(); state.pick() } else { workspace_conn(state, &root).ok_or_else(|| format!("`{name}` has no X-ray database yet: open it once in antiloki, or rebuild it"))? };
    Ok(Space { name, root, open, conn })
}

/// A blocking closure on a space's connection.
async fn on<T: Send + 'static>(sp: &Space, f: impl FnOnce(&Connection, &str) -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let (conn, root) = (sp.conn.clone(), sp.root.clone());
    tokio::task::spawn_blocking(move || { let g = conn.lock().unwrap_or_else(|p| p.into_inner()); f(&g, &root) }).await.map_err(|e| e.to_string())?
}

/// How much of one answer an agent gets: the cut is said, with how to ask for the rest.
const MAX_CHARS: usize = 40_000;
pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max { return text.to_string(); }
    format!("{}\n\n… (cut at {max} of {} characters: ask for less — a `path`, a `limit` or a narrower source)", text.chars().take(max).collect::<String>(), text.chars().count())
}

/// `POST /api/v1/xray/query {space?, source, args?}` -> `{ok, space, summary, content}`: any catalog source of any space as text, for the screen's X-ray pane (the AST, a diagram, any source that has no widget of its own).
pub async fn query_http(state: Arc<AppState>, body: axum::body::Bytes) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse, Json};
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let source = s(&b["source"]); let sargs = if b["args"].is_object() { b["args"].clone() } else { json!({}) };
    let out = async {
        let sp = resolve(&state, &s(&b["space"]))?; let name = sp.name.clone();
        on(&sp, move |db, root| { let generated = antiloki_rs::clock::now_iso(db); antiloki_rs::cockpit__helpers_context_catalog::run(db, root, &source, &sargs, &generated).map(|o| json!({ "ok": true, "space": name, "summary": o.summary, "content": cut(&o.content, MAX_CHARS) })) }).await
    }.await;
    match out { Ok(v) => (StatusCode::OK, Json(v)).into_response(), Err(e) => (StatusCode::OK, Json(json!({ "ok": false, "error": e }))).into_response() }
}

pub async fn run(state: &Arc<AppState>, tool: &str, args: &Value) -> Result<String, String> {
    match tool {
        "antiloki_spaces" => {
            let list = registry(); let mut out = vec![format!("{} space{}:", list.len(), if list.len() == 1 { "" } else { "s" })];
            for (name, root, open) in &list {
                let st = state.clone(); let (n, r) = (name.clone(), root.clone());
                let line = match resolve(&st, &n) {
                    Ok(sp) => on(&sp, move |db, _| {
                        let files = antiloki_rs::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM code_nodes WHERE origin='atlas' AND type='file'", &[]).ok().and_then(|v| v[0]["n"].as_i64()).unwrap_or(0);
                        let rep = antiloki_rs::engine_action_run::skillRunWithContext(&json!({ "action": "system.constitution.report", "input": {} }), db, &json!({ "actor": "mcp", "source": "mcp" })).unwrap_or(Value::Null);
                        let rep = rep.get("result").cloned().unwrap_or(rep); let sm = &rep["summary"];
                        Ok(if sm.is_object() { format!("{files} files · health {} · compliance {} · {} violations · judged {}", sm["health"], sm["compliance"], sm["violations"], sm["created_at"].as_str().unwrap_or("?")) } else if files > 0 { format!("{files} files · not judged yet (rebuild)") } else { "no atlas yet (rebuild)".to_string() })
                    }).await.unwrap_or_else(|e| e),
                    Err(e) => e,
                };
                out.push(format!("- **{n}**{} — `{r}` — {line}", if *open { " (open on the screen)" } else { "" }));
            }
            Ok(out.join("\n"))
        }
        "antiloki_xray" => {
            let source = s(&args["source"]);
            if source.is_empty() || source == "sources" { return Ok(antiloki_rs::cockpit__helpers_context_catalog::catalog_md()); }
            let sargs: Value = { let mut m: Map<String, Value> = args["args"].as_object().cloned().unwrap_or_default(); if let Some(o) = args.as_object() { for (k, v) in o { if !["source", "args", "space"].contains(&k.as_str()) { m.entry(k.clone()).or_insert(v.clone()); } } } Value::Object(m) };
            let sp = resolve(state, &s(&args["space"]))?; let name = sp.name.clone();
            let text = on(&sp, move |db, root| { let generated = antiloki_rs::clock::now_iso(db); antiloki_rs::cockpit__helpers_context_catalog::run(db, root, &source, &sargs, &generated).map(|o| format!("space: {name}\n{}\n\n{}", o.summary, o.content)) }).await?;
            Ok(cut(&text, MAX_CHARS))
        }
        "antiloki_rules" => rules(state, args).await,
        "antiloki_xray_rebuild" => {
            // THE ↻ REBUILD: read the code into the atlas, then JUDGE it against the rules as they are now (a rule you just added or switched only shows its hits after this)
            let sp = resolve(state, &s(&args["space"]))?; let name = sp.name.clone(); let judge = args["judge"] != json!(false);
            let (r, j) = on(&sp, move |db, _| {
                let run = |action: &str| antiloki_rs::engine_action_run::skillRunWithContext(&json!({ "action": action, "input": {} }), db, &json!({ "actor": "mcp", "source": "mcp" }));
                let r = run("system.codebase.refresh")?; let j = if judge { Some(run("system.constitution.judge")?) } else { None };
                Ok((r, j))
            }).await?;
            let r = r.get("result").cloned().unwrap_or(r);
            let judged = j.map(|j| { let j = j.get("result").cloned().unwrap_or(j); let sm = &j["summary"]; format!(" Judged against the rules now in force: {} files, health {}, {} violations.", sm["judged"], sm["health"], sm["violations"]) }).unwrap_or_else(|| " (not judged: `judge` was false).".to_string());
            Ok(format!("rebuilt {name}: {} changed, {} added, {} deleted, {} files re-read, {} edges.{judged}", r["changed"], r["added"], r["deleted"], r["reparsed"], r["edges"]))
        }
        other => Err(format!("unknown X-ray tool `{other}`")),
    }
}

/// The detector kinds a rule can use, and the config each wants: what an agent needs to write one that scans.
pub const DETECTORS_MD: &str = "Detectors (the `detector` field and its `config`), all deterministic:\n\
- `regex` {pattern, flags?} — a match in a file is a violation (flags default `g`; `gm` for line anchors)\n\
- `absent` {pattern} — a file MISSING the pattern is a violation (a required header, a licence)\n\
- `import` {pattern} — an import whose specifier matches the pattern\n\
- `path` {pattern} — a file whose path matches\n\
- `metric` {metric: loc|complexity|fan_in|fan_out|..., max} — a file over the threshold\n\
- `ast` {query / kind} — a tree-sitter structural match (a real `var`, an empty catch), never a word in a string\n\
- `graph` {kind: cycle|god|orphan|...} — a structural smell of the import graph\n\
- `layer` {layers:[{name,paths}], forbid:[[from,to]]} — a forbidden dependency direction between layers\n\
- `linter` {} — the project's own linter findings\n\
- `none` — advisory only (never scored)\n\
Fields: key (slug), title, rule (the sentence), scope (business|architecture|project|framework|community|language), weight 0-10, severity (info|warn|error|critical), enforcement (score|advise|block), applies ({paths:[globs]} to narrow where it runs).";

fn law_input(args: &Value, existing: Option<&Value>) -> Result<Value, String> {
    let pick = |k: &str, alt: &str| -> Value { if !args[k].is_null() { args[k].clone() } else { existing.and_then(|e| e.get(alt)).cloned().unwrap_or(Value::Null) } };
    let key = s(&args["key"]); if key.is_empty() { return Err("`key` is required (a slug: lower-case words and dashes)".into()); }
    let detector = { let d = pick("detector", "detector"); if d.is_null() { json!("none") } else { d } };
    let mut o = Map::new();
    o.insert("op".into(), json!("add")); o.insert("key".into(), json!(key));
    for (k, v) in [("title", pick("title", "title")), ("rule_md", pick("rule", "rule_md")), ("scope", pick("scope", "scope")), ("detector", detector), ("weight", pick("weight", "weight")), ("severity", pick("severity", "severity")), ("enforcement", pick("enforcement", "enforcement")), ("confidence", pick("confidence", "confidence")), ("applies", pick("applies", "applies")), ("config", pick("config", "config"))] { if !v.is_null() { o.insert(k.into(), v); } }
    if existing.is_none() && (!o.contains_key("title") || !o.contains_key("rule_md")) { return Err("a new rule needs `title` and `rule` (the sentence that says what is wrong), and a `detector` with its `config` to scan for it (see op detectors)".into()); }
    if let Some(d) = o.get("detector").and_then(Value::as_str) { if !["regex", "absent", "import", "path", "metric", "ast", "stmt", "graph", "layer", "linter", "none"].contains(&d) { return Err(format!("detector `{d}` is not one of: regex, absent, import, path, metric, ast, stmt, graph, layer, linter, none")); } }
    if let Some(sev) = o.get("severity").and_then(Value::as_str) { if !["info", "warn", "error", "critical"].contains(&sev) { return Err("severity is one of: info, warn, error, critical".into()); } }
    Ok(Value::Object(o))
}

async fn rules(state: &Arc<AppState>, args: &Value) -> Result<String, String> {
    let op = { let o = s(&args["op"]); if o.is_empty() { "list".to_string() } else { o } };
    if op == "detectors" { return Ok(DETECTORS_MD.to_string()); }
    let sp = resolve(state, &s(&args["space"]))?; let name = sp.name.clone();
    if op == "list" || op == "show" {
        let a = args.clone(); let key = s(&args["key"]); let show = op == "show";
        let text = on(&sp, move |db, root| {
            let generated = antiloki_rs::clock::now_iso(db);
            let o = antiloki_rs::cockpit__helpers_context_catalog::run(db, root, "rules", &json!({ "status": s(&a["status"]) }), &generated)?;
            if show { let v: Value = serde_json::from_str(o.content.split_once("\n\n").map(|x| x.1).unwrap_or(&o.content)).unwrap_or(Value::Null); let hit = v["rules"].as_array().and_then(|r| r.iter().find(|x| s(&x["key"]) == key)).cloned().ok_or_else(|| format!("no rule `{key}`: antiloki_rules list"))?; return Ok(serde_json::to_string_pretty(&hit).unwrap_or_default()); }
            Ok(format!("{}\n\n{}", o.summary, o.content))
        }).await?;
        return Ok(format!("space: {name}\n{}", cut(&text, MAX_CHARS)));
    }
    let keys: Vec<String> = if let Some(a) = args["keys"].as_array() { a.iter().map(s).filter(|k| !k.is_empty()).collect() } else if !s(&args["key"]).is_empty() { vec![s(&args["key"])] } else { vec![] };
    let a = args.clone();
    let done = on(&sp, move |db, _| {
        let law = |input: Value| antiloki_rs::constitution_constitution_law::constitutionLaw(&input, db);
        let mut said: Vec<String> = vec![];
        match op.as_str() {
            "add" | "edit" => {
                let key = s(&a["key"]);
                let existing = law(json!({ "op": "list" }))?["laws"].as_array().and_then(|l| l.iter().find(|x| s(&x["key"]) == key)).cloned();
                if op == "add" && existing.is_some() { return Err(format!("`{key}` already exists: use op edit")); }
                if op == "edit" && existing.is_none() { return Err(format!("no rule `{key}` to edit: antiloki_rules list")); }
                let r = law(law_input(&a, existing.as_ref())?)?;
                said.push(format!("{} `{}` ({}).", if existing.is_some() { "edited" } else { "added" }, s(&r["key"]), s(&r["scope"])));
            }
            "activate" | "deactivate" | "remove" | "ratify" | "dismiss" | "reweight" => {
                if keys.is_empty() { return Err("`key` (or a list `keys`) is required".into()); }
                let backend = match op.as_str() { "activate" => "unsuspend", "deactivate" => "suspend", o => o };
                for k in &keys {
                    let mut input = json!({ "op": backend, "key": k });
                    if op == "reweight" { if a["weight"].is_null() { return Err("`weight` (0-10) is required".into()); } input["weight"] = a["weight"].clone(); }
                    law(input)?;
                    let past = match op.as_str() { "activate" => "activated", "deactivate" => "deactivated", "remove" => "removed", "ratify" => "ratified", "dismiss" => "dismissed", _ => "reweighted" };
                    said.push(format!("{past} `{k}`."));
                }
            }
            "except" => { law(json!({ "op": "except", "key": s(&a["key"]), "path_glob": s(&a["path_glob"]), "reason": s(&a["reason"]), "created_by": "agent" }))?; said.push(format!("`{}` no longer applies to `{}`.", s(&a["key"]), s(&a["path_glob"]))); }
            "unexcept" => { law(json!({ "op": "unexcept", "exception_id": a["exception_id"] }))?; said.push("exception removed.".into()); }
            "seed" => { let r = law(json!({ "op": "seed" }))?; said.push(format!("the standard rules are in place: {r}")); }
            other => return Err(format!("op is one of: list, show, add, edit, activate, deactivate, remove, reweight, except, unexcept, ratify, dismiss, seed, detectors (not `{other}`)")),
        }
        Ok(said)
    }).await?;
    Ok(format!("space: {name}\n{}\nA rule scans the code when the space is rebuilt: antiloki_xray_rebuild (or the ↻ rebuild in X-ray) — until then its hits read as they were.", done.join("\n")))
}

/// The X-ray views a pane can show, and what each needs. The screen renders these; the names are the contract.
pub const VIEWS: [(&str, &str); 20] = [
    ("board", "X-ray v2: the board of widgets (declarations drawn by fixed renderers): arrange, add, install; the same board antiloki_xray_layout edits"),
    ("overview", "the Overview tab: verdict, hotspots, the codebase map, ownership, debt, activity"),
    ("file", "a file's code review: the lines, the comments, the axes (needs `path`)"),
    ("folder", "a folder's paper: the long deterministic document (needs `path`)"),
    ("code", "the code reader on a file (needs `path`)"),
    ("ast", "a file's syntax tree and definitions (needs `path`; optional `symbol`)"),
    ("file-diagram", "a file's internal call/flow diagram (needs `path`)"),
    ("system-diagram", "the system diagram: boxes by folder and the arrows between them (optional `path`, `depth`)"),
    ("import-graph", "the import graph and its blast radius (optional `path`)"),
    ("data-stories", "the data stories: entry -> every file crossed -> death point"),
    ("rules", "the Quality tab's rules (switch, edit, add)"),
    ("constitution", "the constitution: law health, axes, violations by law, worst spots"),
    ("coverage", "the coverage x-ray"),
    ("health-over-time", "architecture over time: health, cycles, coverage per rebuild"),
    ("review-history", "the review history: movers and axis drift"),
    ("working-tree", "the working-tree diff"),
    ("git-graph", "the git graph: branches and history"),
    ("authors", "authors: who wrote what, bus factor"),
    ("design", "the Design tab: system, components, frames"),
    ("telemetry", "live signals: AI spend, latency, the rebuild's phase timings"),
];

/// Validate an `antiloki_xray_open` request and shape the screen command. Pure.
pub fn open_command(args: &Value) -> Result<Value, String> {
    let view = s(&args["view"]);
    let Some((_, _what)) = VIEWS.iter().find(|(k, _)| *k == view) else { return Err(format!("`view` is one of: {}", VIEWS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", "))); };
    let path = s(&args["path"]).replace('\\', "/").trim_matches('/').to_string();
    if ["file", "folder", "code", "ast", "file-diagram"].contains(&view.as_str()) && path.is_empty() { return Err(format!("view `{view}` needs a `path`")); }
    if path.split('/').any(|p| p == "..") { return Err("`path` is inside the space: no `..`".into()); }
    let mut c = json!({ "kind": "studio", "tool": "xray-view", "view": view, "path": path, "space": s(&args["space"]), "title": s(&args["title"]), "side": s(&args["side"]) });
    for k in ["symbol", "depth"] { if !args[k].is_null() { c[k] = args[k].clone(); } }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<(String, String, bool)> { vec![("alpha".into(), "/p/alpha".into(), true), ("Beta".into(), "/p/beta".into(), false)] }

    #[test]
    fn a_space_is_named_by_name_or_root_and_nothing_means_the_open_one() {
        assert_eq!(pick_space(&list(), "").unwrap().0, "alpha");
        assert_eq!(pick_space(&list(), "beta").unwrap().1, "/p/beta", "any case");
        assert_eq!(pick_space(&list(), "/p/beta/").unwrap().0, "Beta", "by root, trailing slash or not");
        let e = pick_space(&list(), "gamma").unwrap_err(); assert!(e.contains("alpha, Beta"), "{e}");
        assert!(pick_space(&[], "").unwrap_err().contains("antiloki_spaces"));
    }

    #[test]
    fn a_long_answer_is_cut_with_the_cut_said() {
        assert_eq!(cut("short", 100), "short");
        let c = cut(&"x".repeat(50), 10); assert!(c.starts_with("xxxxxxxxxx\n") && c.contains("cut at 10 of 50"), "{c}");
    }

    #[test]
    fn a_new_rule_needs_its_sentence_and_a_known_detector_and_an_edit_keeps_what_is_not_changed() {
        assert!(law_input(&json!({}), None).unwrap_err().contains("`key`"));
        assert!(law_input(&json!({ "key": "no-x" }), None).unwrap_err().contains("`title` and `rule`"));
        assert!(law_input(&json!({ "key": "no-x", "title": "t", "rule": "r", "detector": "magic" }), None).unwrap_err().contains("not one of"));
        assert!(law_input(&json!({ "key": "no-x", "title": "t", "rule": "r", "severity": "meh" }), None).unwrap_err().contains("severity"));
        let new = law_input(&json!({ "key": "no-x", "title": "No x", "rule": "x is bad", "detector": "regex", "config": { "pattern": "x" }, "weight": 4 }), None).unwrap();
        assert_eq!((new["op"].clone(), new["detector"].clone(), new["config"]["pattern"].clone(), new["weight"].clone()), (json!("add"), json!("regex"), json!("x"), json!(4)));
        let was = json!({ "key": "no-x", "title": "No x", "rule_md": "x is bad", "scope": "project", "detector": "regex", "config": { "pattern": "x" }, "weight": 4, "severity": "warn" });
        let edit = law_input(&json!({ "key": "no-x", "weight": 9 }), Some(&was)).unwrap();
        assert_eq!((edit["weight"].clone(), edit["title"].clone(), edit["rule_md"].clone(), edit["config"]["pattern"].clone(), edit["severity"].clone()), (json!(9), json!("No x"), json!("x is bad"), json!("x"), json!("warn")), "only the weight moved");
    }

    #[test]
    fn opening_a_view_names_a_known_view_and_a_path_inside_the_space() {
        assert!(open_command(&json!({ "view": "nope" })).unwrap_err().contains("system-diagram"));
        assert!(open_command(&json!({ "view": "file" })).unwrap_err().contains("needs a `path`"));
        assert!(open_command(&json!({ "view": "folder", "path": "../etc" })).unwrap_err().contains("no `..`"));
        let c = open_command(&json!({ "view": "ast", "path": "/src/a.ts/", "symbol": "go", "space": "beta", "side": "right" })).unwrap();
        assert_eq!((c["kind"].clone(), c["tool"].clone(), c["view"].clone(), c["path"].clone(), c["space"].clone(), c["symbol"].clone()), (json!("studio"), json!("xray-view"), json!("ast"), json!("src/a.ts"), json!("beta"), json!("go")));
        assert!(open_command(&json!({ "view": "rules" })).is_ok() && open_command(&json!({ "view": "system-diagram", "depth": 3 })).unwrap()["depth"] == json!(3));
    }
}
