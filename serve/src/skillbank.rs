//! OUR OWN SKILLSHARE (operator 2026-11-10: "the skillshare pane still asking to install skillshare: no, we will have our own skillshare-like feature on skills, not using others' project").
//!
//! One BANK of skills and agents, kept by antiloki, and every AI tool on the machine pointed at it:
//!   bank       `~/.antiloki/skillbank/{skills/<name>/SKILL.md…, agents/<name>.md}`   (a project has its own: `<project>/.antiloki/skillbank/`)
//!   targets    each tool's own folder: Claude Code `~/.claude/skills`, Codex `~/.codex/skills`, Antigravity `~/.gemini/skills` (in a project: `.agents/skills`), Cursor `~/.cursor/skills`, OpenCode `~/.config/opencode/skills` (agents likewise where the tool has them)
//!   a sync     makes a SYMLINK in each enabled target to the bank's folder, so editing a skill once changes it for every tool; nothing the person already has there is overwritten (it is reported as "exists, not managed")
//!   a source   `owner/repo` or a git address: installing clones it (shallow, no scripts run), copies the skills and agents it holds into the bank and remembers where from; `check` asks the remote whether it moved; `update` fetches it again
//! A target is enabled when the tool's folder exists on this machine. Nothing is installed or executed from a source: it is files copied, then linked.
//! The state is given to the screen in the same shape the Skills pane already reads (`list`, `diff`, `check`), so the pane is the same pane.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct Target { pub key: &'static str, pub label: &'static str, pub marker: &'static str, pub skills: &'static str, pub agents: Option<&'static str> }
/// Where each tool looks for skills and agents, relative to the home folder (global) or to a project (the same names under the project, without the leading config folder rules of the tool).
pub const TARGETS: [Target; 5] = [
    Target { key: "claude", label: "Claude Code", marker: ".claude", skills: ".claude/skills", agents: Some(".claude/agents") },
    Target { key: "codex", label: "Codex", marker: ".codex", skills: ".codex/skills", agents: None },
    Target { key: "gemini", label: "Antigravity", marker: ".gemini", skills: ".gemini/skills", agents: None },
    Target { key: "cursor", label: "Cursor", marker: ".cursor", skills: ".cursor/skills", agents: Some(".cursor/agents") },
    Target { key: "opencode", label: "OpenCode", marker: ".config/opencode", skills: ".config/opencode/skills", agents: Some(".config/opencode/agents") },
];
const MAX_BYTES: u64 = 20 * 1024 * 1024;

/// Where one scope keeps its bank and its targets: global = the home folder, project = the project's folder.
#[derive(Clone, Debug)]
pub struct Scope { pub bank: PathBuf, pub base: PathBuf, pub project: bool }
impl Scope {
    pub fn global(home: &Path) -> Scope { Scope { bank: home.join(".antiloki/skillbank"), base: home.to_path_buf(), project: false } }
    pub fn project(root: &Path) -> Scope { Scope { bank: root.join(".antiloki/skillbank"), base: root.to_path_buf(), project: true } }
    pub fn skills_dir(&self) -> PathBuf { self.bank.join("skills") }
    pub fn agents_dir(&self) -> PathBuf { self.bank.join("agents") }
    fn meta_path(&self) -> PathBuf { self.bank.join("meta.json") }
    /// The tools this scope links into: for the global scope those whose folder exists in the home; for a project, the same tools (their folders are made there).
    pub fn targets(&self, home: &Path) -> Vec<&'static Target> { TARGETS.iter().filter(|t| home.join(t.marker).exists()).collect() }
}

/// A name that is one folder or file name and nothing else: letters, digits, `.`, `_`, `-`; no leading dot or dash. Pure.
pub fn safe_item(n: &str) -> bool { let n = n.trim(); !n.is_empty() && n.len() <= 80 && !n.starts_with(['.', '-']) && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) }
/// A source to install from: `owner/repo`, or a git address (https, ssh, git@). A local path or file address only when `allow_file` (the tests). Never a dash first, a space, or a shell character. Pure.
pub fn safe_source(src: &str, allow_file: bool) -> bool {
    let t = src.trim();
    if t.is_empty() || t.len() > 300 || t.starts_with('-') || t.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '`' | '$' | ';' | '&' | '|' | '<' | '>' | '\\' | '(' | ')' | '*' | '?' | '{' | '}')) { return false; }
    if t.starts_with("https://") || t.starts_with("http://") || t.starts_with("ssh://") || t.starts_with("git@") { return true; }
    if t.starts_with("file://") { return allow_file; }
    if t.starts_with('/') || t.starts_with('~') { return false; }
    let parts: Vec<&str> = t.split('/').collect(); parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && *p != ".." && !p.starts_with('.'))
}
/// The git address of a source: `owner/repo` is on GitHub.
pub fn git_url(src: &str) -> String { let t = src.trim(); if t.contains("://") || t.starts_with("git@") { t.to_string() } else { format!("https://github.com/{t}.git") } }

fn read_meta(s: &Scope) -> Value { std::fs::read_to_string(s.meta_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).filter(|v: &Value| v.is_object()).unwrap_or_else(|| json!({})) }
fn write_meta(s: &Scope, v: &Value) -> Result<(), String> { std::fs::create_dir_all(&s.bank).map_err(|e| e.to_string())?; let tmp = s.meta_path().with_extension("tmp"); std::fs::write(&tmp, v.to_string()).map_err(|e| e.to_string())?; std::fs::rename(&tmp, s.meta_path()).map_err(|e| e.to_string()) }
fn meta_key(kind: &str, name: &str) -> String { format!("{kind}:{name}") }

/// One thing in the bank.
#[derive(Clone, Debug, PartialEq)]
pub struct Item { pub kind: &'static str, pub name: String, pub disabled: bool, pub source: Option<String>, pub commit: Option<String> }
/// What the bank holds: skills (folders), agents (`.md` files). Sorted by name.
pub fn items(s: &Scope) -> Vec<Item> {
    let meta = read_meta(s); let mut out = vec![];
    let mk = |kind: &'static str, name: String| { let m = &meta[meta_key(kind, &name)]; Item { kind, name, disabled: m["disabled"] == json!(true), source: m["source"].as_str().map(String::from), commit: m["commit"].as_str().map(String::from) } };
    if let Ok(rd) = std::fs::read_dir(s.skills_dir()) { for e in rd.flatten() { let n = e.file_name().to_string_lossy().to_string(); if safe_item(&n) && e.path().is_dir() { out.push(mk("skill", n)); } } }
    if let Ok(rd) = std::fs::read_dir(s.agents_dir()) { for e in rd.flatten() { let n = e.file_name().to_string_lossy().to_string(); if n.ends_with(".md") && safe_item(&n) && e.path().is_file() { out.push(mk("agent", n)); } } }
    out.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name))); out
}
fn bank_path(s: &Scope, i: &Item) -> PathBuf { if i.kind == "agent" { s.agents_dir().join(&i.name) } else { s.skills_dir().join(&i.name) } }
/// Where a tool keeps this kind of thing for this scope (the tool's folder under the home or the project), or None when the tool has none (an agent for Codex).
/// Antigravity (the `gemini` seat) reads its GLOBAL skills from `~/.gemini/skills` (among others) but its PROJECT skills from `<project>/.agents/skills`, not from a `.gemini` folder: that one is the old Gemini CLI's.
pub const ANTIGRAVITY_PROJECT_SKILLS: &str = ".agents/skills";
fn target_dir(s: &Scope, t: &Target, kind: &str) -> Option<PathBuf> { if kind == "agent" { t.agents.map(|a| s.base.join(a)) } else if s.project && t.key == "gemini" { Some(s.base.join(ANTIGRAVITY_PROJECT_SKILLS)) } else { Some(s.base.join(t.skills)) } }

/// What a target has at an item's place.
#[derive(Debug, PartialEq, Clone)]
pub enum Slot { Absent, Linked, StaleLink, Foreign }
pub fn slot(link: &Path, bank: &Path) -> Slot {
    match std::fs::symlink_metadata(link) {
        Err(_) => Slot::Absent,
        Ok(m) if m.file_type().is_symlink() => match std::fs::read_link(link) { Ok(t) if t == bank => Slot::Linked, Ok(t) if t.starts_with(bank.parent().and_then(|p| p.parent()).unwrap_or(bank)) => Slot::StaleLink, Ok(t) if !t.exists() && t.to_string_lossy().contains("skillbank") => Slot::StaleLink, _ => Slot::Foreign },
        Ok(_) => Slot::Foreign,
    }
}

/// What a sync would do, per target: `(target key, item, action, reason)`. `add` = make the link; `remove` = a link into the bank that should not be there (the item is disabled or gone); `skip` = something else is in the place.
pub fn plan(s: &Scope, home: &Path) -> Vec<(String, String, String, &'static str, String)> {
    let its = items(s); let mut out = vec![];
    for t in s.targets(home) {
        for i in &its {
            let Some(dir) = target_dir(s, t, i.kind) else { continue }; let link = dir.join(&i.name); let b = bank_path(s, i);
            match (slot(&link, &b), i.disabled) {
                (Slot::Linked, false) => {}
                (Slot::Linked, true) | (Slot::StaleLink, _) => out.push((t.key.to_string(), i.name.clone(), "remove".into(), i.kind, "it is switched off".to_string())),
                (Slot::Absent, false) => out.push((t.key.to_string(), i.name.clone(), "add".into(), i.kind, "not linked yet".to_string())),
                (Slot::Absent, true) => {}
                (Slot::Foreign, false) => out.push((t.key.to_string(), i.name.clone(), "skip".into(), i.kind, format!("{} already has its own, which antiloki does not touch", t.label))),
                (Slot::Foreign, true) => {}
            }
        }
        // a link into the bank for something that is no longer in it
        for (kind, dir) in [("skill", target_dir(s, t, "skill")), ("agent", target_dir(s, t, "agent"))] {
            let Some(dir) = dir else { continue }; let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() { let n = e.file_name().to_string_lossy().to_string(); if its.iter().any(|i| i.kind == kind && i.name == n) { continue; }
                if let Ok(tg) = std::fs::read_link(e.path()) { let bank_kind_dir = if kind == "agent" { s.agents_dir() } else { s.skills_dir() }; if tg.starts_with(&bank_kind_dir) { out.push((t.key.to_string(), n, "remove".into(), if kind == "agent" { "agent" } else { "skill" }, "it is not in the bank any more".to_string())); } } }
        }
    }
    out
}
/// Make every target match the bank. Returns (changed, skipped lines).
pub fn sync(s: &Scope, home: &Path) -> Result<(usize, Vec<String>), String> {
    let mut changed = 0; let mut skipped = vec![]; let its = items(s);
    for (tk, name, action, kind, reason) in plan(s, home) {
        let Some(t) = TARGETS.iter().find(|t| t.key == tk) else { continue }; let Some(dir) = target_dir(s, t, kind) else { continue }; let link = dir.join(&name);
        match action.as_str() {
            "add" => { let it = its.iter().find(|i| i.name == name && i.kind == kind).ok_or("the item is gone")?; std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?; symlink(&bank_path(s, it), &link).map_err(|e| format!("could not link {name} into {}: {e}", t.label))?; changed += 1; }
            "remove" => { if std::fs::symlink_metadata(&link).map(|m| m.file_type().is_symlink()).unwrap_or(false) { std::fs::remove_file(&link).map_err(|e| e.to_string())?; changed += 1; } }
            _ => skipped.push(format!("{} · {name}: {reason}", t.label)),
        }
    }
    Ok((changed, skipped))
}
#[cfg(unix)] fn symlink(from: &Path, to: &Path) -> std::io::Result<()> { std::os::unix::fs::symlink(from, to) }
#[cfg(not(unix))] fn symlink(from: &Path, to: &Path) -> std::io::Result<()> { if from.is_dir() { copy_dir(from, to).map(|_| ()).map_err(|e| std::io::Error::other(e)) } else { std::fs::copy(from, to).map(|_| ()) } }

/// Copy a folder, leaving out `.git` and every symlink (a source must not smuggle a link out of itself), and refusing a tree over the size limit.
pub fn copy_dir(from: &Path, to: &Path) -> Result<u64, String> {
    fn go(from: &Path, to: &Path, total: &mut u64) -> Result<(), String> {
        std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
        for e in std::fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
            let name = e.file_name(); if name == ".git" { continue; } let m = std::fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?; if m.file_type().is_symlink() { continue; }
            if m.is_dir() { go(&e.path(), &to.join(&name), total)?; } else { *total += m.len(); if *total > MAX_BYTES { return Err("that is more than 20 MB: not a skill".into()); } std::fs::copy(e.path(), to.join(&name)).map_err(|e| e.to_string())?; }
        }
        Ok(())
    }
    let mut total = 0; go(from, to, &mut total)?; Ok(total)
}

/// What a cloned source holds: skill folders (a `SKILL.md` inside, at the root, under `skills/`, or one level deeper) and agent files (`agents/*.md`). Pure over the folder.
pub fn discover(root: &Path) -> (Vec<(String, PathBuf)>, Vec<(String, PathBuf)>) {
    let mut skills: Vec<(String, PathBuf)> = vec![]; let mut agents = vec![];
    if root.join("SKILL.md").is_file() { skills.push((root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), root.to_path_buf())); }
    fn walk(d: &Path, depth: u8, out: &mut Vec<(String, PathBuf)>) { if let Ok(rd) = std::fs::read_dir(d) { for e in rd.flatten() { let p = e.path(); let n = e.file_name().to_string_lossy().to_string(); if n == ".git" || !p.is_dir() { continue; } if p.join("SKILL.md").is_file() { out.push((n, p)); } else if depth > 0 { walk(&p, depth - 1, out); } } } }
    walk(&root.join("skills"), 1, &mut skills);
    if skills.iter().all(|(_, p)| p == root) { walk(root, 1, &mut skills); }
    if let Ok(rd) = std::fs::read_dir(root.join("agents")) { for e in rd.flatten() { let n = e.file_name().to_string_lossy().to_string(); if n.ends_with(".md") && e.path().is_file() { agents.push((n, e.path())); } } }
    skills.sort(); skills.dedup_by(|a, b| a.0 == b.0); agents.sort(); (skills, agents)
}
/// Put what a source holds into the bank. Returns the names added. An existing name of another origin is refused (nothing is overwritten that the person made).
pub fn install_from_dir(s: &Scope, dir: &Path, source: &str, commit: Option<&str>, replace: bool) -> Result<Vec<String>, String> {
    let (skills, agents) = discover(dir); if skills.is_empty() && agents.is_empty() { return Err("no skills or agents in that source (a skill is a folder with a SKILL.md)".into()); }
    let mut meta = read_meta(s); let mut added = vec![];
    for (name, path) in &skills {
        let name = if safe_item(name) { name.clone() } else { return Err(format!("`{name}` is not a name a skill can have here")); }; let dest = s.skills_dir().join(&name);
        let known = meta[meta_key("skill", &name)]["source"] == json!(source);
        if dest.exists() { if !(replace && known) { return Err(format!("a skill called `{name}` is already in the bank{}", if known { " (from this source: use update)" } else { " and it is not from this source: it is not overwritten" })); } std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?; }
        copy_dir(path, &dest)?; meta[meta_key("skill", &name)] = json!({ "source": source, "commit": commit, "installed_ms": crate::cli::now_ms() }); added.push(name);
    }
    for (name, path) in &agents {
        if !safe_item(name) { return Err(format!("`{name}` is not a name an agent can have here")); } let dest = s.agents_dir().join(name); let known = meta[meta_key("agent", name)]["source"] == json!(source);
        if dest.exists() { if !(replace && known) { return Err(format!("an agent called `{name}` is already in the bank{}", if known { " (from this source: use update)" } else { " and it is not from this source: it is not overwritten" })); } }
        std::fs::create_dir_all(s.agents_dir()).map_err(|e| e.to_string())?; std::fs::copy(path, &dest).map_err(|e| e.to_string())?; meta[meta_key("agent", name)] = json!({ "source": source, "commit": commit, "installed_ms": crate::cli::now_ms() }); added.push(name.clone());
    }
    write_meta(s, &meta)?; Ok(added)
}
/// A new skill of the person's own: a folder with a SKILL.md they can edit.
pub fn new_skill(s: &Scope, name: &str, description: &str) -> Result<(), String> {
    if !safe_item(name) { return Err("a skill's name is letters, digits, dots, dashes and underscores".into()); } let dir = s.skills_dir().join(name); if dir.exists() { return Err(format!("a skill called `{name}` is already in the bank")); }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?; let d: String = description.lines().next().unwrap_or("").trim().chars().take(300).collect();
    std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: {}\n---\n\n# {name}\n\nWhat this skill is for, and how the agent should do it.\n", if d.is_empty() { "When to use this skill" } else { &d })).map_err(|e| e.to_string())
}
/// Take an item out of the bank (and its links with the next sync). A folder is removed whole.
pub fn remove(s: &Scope, kind: &str, name: &str) -> Result<(), String> {
    if !safe_item(name) { return Err("that name is not one the bank lists".into()); } let p = if kind == "agent" { s.agents_dir().join(name) } else { s.skills_dir().join(name) };
    if !p.exists() { return Err("it is not in the bank".into()); } if p.is_dir() { std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?; } else { std::fs::remove_file(&p).map_err(|e| e.to_string())?; }
    let mut meta = read_meta(s); if let Some(o) = meta.as_object_mut() { o.remove(&meta_key(kind, name)); } write_meta(s, &meta)
}
pub fn set_disabled(s: &Scope, kind: &str, name: &str, off: bool) -> Result<(), String> {
    if !safe_item(name) || !items(s).iter().any(|i| i.kind == kind && i.name == name) { return Err("it is not in the bank".into()); }
    let mut meta = read_meta(s); meta[meta_key(kind, name)]["disabled"] = json!(off); write_meta(s, &meta)
}

// ── git ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

fn git(args: &[&str], cwd: Option<&Path>, secs: u64) -> Result<String, String> {
    let mut c = crate::proc::cmd("git"); c.args(["-c", "core.symlinks=false", "-c", "protocol.ext.allow=never", "-c", "core.hooksPath=/dev/null"]).args(args).env("GIT_TERMINAL_PROMPT", "0").env("GIT_ASKPASS", "echo").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    if let Some(d) = cwd { c.current_dir(d); }
    let mut child = c.spawn().map_err(|e| format!("could not run git: {e}"))?; let t0 = std::time::Instant::now();
    loop { match child.try_wait() { Ok(Some(_)) => break, Ok(None) if t0.elapsed().as_secs() > secs => { let _ = child.kill(); let _ = child.wait(); return Err(format!("git took longer than {secs} seconds")); } Ok(None) => std::thread::sleep(std::time::Duration::from_millis(40)), Err(e) => return Err(e.to_string()) } }
    let o = child.wait_with_output().map_err(|e| e.to_string())?; if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).to_string()) } else { Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("git failed").chars().take(200).collect()) }
}
/// The commit a remote's HEAD is at.
pub fn remote_head(url: &str) -> Result<String, String> { git(&["ls-remote", url, "HEAD"], None, 25).map(|o| o.split_whitespace().next().unwrap_or("").to_string()).and_then(|c| if c.is_empty() { Err("the remote has no HEAD".into()) } else { Ok(c) }) }
/// Clone a source (shallow) into a fresh temp folder; returns (folder, commit).
pub fn clone_source(url: &str) -> Result<(PathBuf, String), String> {
    let d = std::env::temp_dir().join(format!("antiloki-skillsrc-{}-{}", std::process::id(), crate::cli::now_ms())); let _ = std::fs::remove_dir_all(&d);
    git(&["clone", "--depth", "1", "--no-recurse-submodules", "--quiet", url, &d.to_string_lossy()], None, 120)?;
    let commit = git(&["rev-parse", "HEAD"], Some(&d), 10).map(|s| s.trim().to_string()).unwrap_or_default(); Ok((d, commit))
}

// ── what the screen reads ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

fn allow_file() -> bool { std::env::var("ANTILOKI_SKILLBANK_ALLOW_FILE").map_or(false, |v| v == "1") }
/// The state of one scope in the shape the pane reads: `list`, `diff` (what a sync would do), and with `checks` the update check.
pub fn scope_state(s: &Scope, home: &Path, checks: bool) -> Value {
    let its = items(s);
    let list: Vec<Value> = its.iter().map(|i| json!({ "name": i.name, "kind": i.kind, "relPath": i.name, "disabled": i.disabled, "source": i.source })).collect();
    let mut by: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    for (tk, name, action, kind, reason) in plan(s, home) { by.entry(tk).or_default().push(json!({ "kind": kind, "name": name, "action": if action == "skip" { "skip".to_string() } else { action.clone() }, "reason": reason, "is_sync": action != "skip" })); }
    let diff = json!({ "targets": by.into_iter().map(|(n, items)| json!({ "name": n, "items": items })).collect::<Vec<_>>() });
    let mut o = json!({ "list": list, "diff": diff, "mcp": [], "plugins": json!({}), "hooks": json!({}), "error": Value::Null });
    if checks {
        let mut sk = vec![]; let mut ag = vec![]; let mut heads: std::collections::HashMap<String, Option<String>> = Default::default();
        for i in &its { let (Some(src), Some(commit)) = (&i.source, &i.commit) else { continue }; let url = git_url(src);
            let head = heads.entry(url.clone()).or_insert_with(|| remote_head(&url).ok()).clone();
            if let Some(h) = head { if &h != commit { let row = json!({ "name": i.name.trim_end_matches(".md"), "status": "update_available", "source": src }); if i.kind == "agent" { ag.push(row); } else { sk.push(row); } } } }
        o["check"] = json!({ "skills": sk, "tracked_repos": [] }); o["checkAgents"] = json!(ag);
    }
    o
}
/// The whole state: the two scopes, the bank's place, how many tools it links to.
pub fn read_state(home: &Path, project_root: Option<&Path>, checks: bool) -> Value {
    let g = Scope::global(home); let mut scopes = serde_json::Map::new(); scopes.insert("global".into(), scope_state(&g, home, checks));
    if let Some(r) = project_root { let p = Scope::project(r); if p.bank.exists() || p.skills_dir().exists() { scopes.insert("project".into(), scope_state(&p, home, checks)); } }
    json!({ "ok": true, "installed": true, "native": true, "version": "antiloki", "source": g.bank.to_string_lossy(), "targets": g.targets(home).len(), "tools": g.targets(home).iter().map(|t| json!({ "key": t.key, "label": t.label })).collect::<Vec<_>>(), "scopes": scopes })
}

/// One button. Blocking. `req`: `{op, scope, name?, kind?, source?, description?, all?}`.
pub fn perform(home: &Path, project_root: Option<&Path>, req: &Value) -> Value {
    let op = req["op"].as_str().unwrap_or("").to_string(); let project = req["scope"] == "project";
    let s = if project { match project_root { Some(r) => Scope::project(r), None => return json!({ "ok": false, "error": "no project is open" }) } } else { Scope::global(home) };
    let kind = if req["kind"] == "agents" || req["kind"] == "agent" { "agent" } else { "skill" }; let name = req["name"].as_str().unwrap_or("").trim().to_string();
    let fail = |e: String| json!({ "ok": false, "op": op, "error": e });
    let done = |extra: Value| { let mut v = json!({ "ok": true, "op": op, "scope": if project { "project" } else { "global" } }); if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) { for (k, x) in e { o.insert(k.clone(), x.clone()); } } v };
    match op.as_str() {
        "sync" => match sync(&s, home) { Ok((n, skipped)) => done(json!({ "json": { "synced": n, "skipped": skipped } })), Err(e) => fail(e) },
        "check" => done(json!({ "json": scope_state(&s, home, true)["check"] })),
        "new" => match new_skill(&s, &name, req["description"].as_str().unwrap_or("")).and_then(|_| sync(&s, home).map(|_| ())) { Ok(()) => done(json!({ "json": { "created": name } })), Err(e) => fail(e) },
        "uninstall" => match remove(&s, kind, &if kind == "agent" && !name.ends_with(".md") { format!("{name}.md") } else { name.clone() }).and_then(|_| sync(&s, home).map(|_| ())) { Ok(()) => done(json!({ "json": { "removed": name } })), Err(e) => fail(e) },
        "hook" => Err::<(), String>("not here".into()).map_or_else(fail, |_| Value::Null),
        "toggle" => { let n = if kind == "agent" && !name.ends_with(".md") { format!("{name}.md") } else { name.clone() }; match set_disabled(&s, kind, &n, req["off"] == json!(true)).and_then(|_| sync(&s, home).map(|_| ())) { Ok(()) => done(json!({})), Err(e) => fail(e) } }
        "install" | "update" => {
            let (source, replace, names): (String, bool, Vec<String>) = if op == "install" { (req["source"].as_str().unwrap_or("").trim().to_string(), false, vec![]) } else {
                let its = items(&s); let pick: Vec<&Item> = if req["all"] == json!(true) { its.iter().filter(|i| i.source.is_some()).collect() } else { its.iter().filter(|i| i.source.is_some() && (i.name == name || i.name.trim_end_matches(".md") == name) && i.kind == kind).collect() };
                let Some(first) = pick.first() else { return fail("nothing to update: it has no source (it is a skill of your own)".into()) };
                if req["all"] == json!(true) { let mut srcs: Vec<String> = pick.iter().filter_map(|i| i.source.clone()).collect(); srcs.sort(); srcs.dedup(); return updates(&s, home, srcs, &op, project); }
                (first.source.clone().unwrap_or_default(), true, pick.iter().map(|i| i.name.clone()).collect())
            };
            let _ = names; if !safe_source(&source, allow_file()) { return fail("a source is owner/repo or a git address (https, ssh, git@)".into()); }
            match clone_source(&git_url(&source)).and_then(|(d, c)| { let r = install_from_dir(&s, &d, &source, Some(&c), replace); let _ = std::fs::remove_dir_all(&d); r }).and_then(|added| sync(&s, home).map(|(n, sk)| (added, n, sk))) { Ok((added, n, skipped)) => done(json!({ "json": { "installed": added, "synced": n, "skipped": skipped } })), Err(e) => fail(e) }
        }
        other => fail(format!("`{other}` is not something the pane does (sync, check, update, uninstall, install, new, toggle)")),
    }
}
fn updates(s: &Scope, home: &Path, sources: Vec<String>, op: &str, project: bool) -> Value {
    let mut updated = vec![]; let mut errors = vec![];
    for src in sources { if !safe_source(&src, allow_file()) { errors.push(format!("{src}: not a source antiloki will fetch")); continue; }
        match clone_source(&git_url(&src)).and_then(|(d, c)| { let r = install_from_dir(s, &d, &src, Some(&c), true); let _ = std::fs::remove_dir_all(&d); r }) { Ok(a) => updated.extend(a), Err(e) => errors.push(format!("{src}: {e}")) } }
    let _ = sync(s, home); json!({ "ok": errors.is_empty(), "op": op, "scope": if project { "project" } else { "global" }, "json": { "updated": updated }, "error": if errors.is_empty() { Value::Null } else { json!(errors.join("; ")) } })
}

#[cfg(test)]
mod tests {
    #[test]
    fn antigravity_reads_project_skills_from_dot_agents_and_global_ones_from_dot_gemini() {
        let g = Target { key: "gemini", label: "Antigravity", marker: ".gemini", skills: ".gemini/skills", agents: None };
        let global = Scope::global(Path::new("/home/u")); let project = Scope::project(Path::new("/work/app"));
        assert_eq!(target_dir(&global, &g, "skill").unwrap(), PathBuf::from("/home/u/.gemini/skills"));
        assert_eq!(target_dir(&project, &g, "skill").unwrap(), PathBuf::from("/work/app/.agents/skills"));
        let c = Target { key: "claude", label: "Claude Code", marker: ".claude", skills: ".claude/skills", agents: Some(".claude/agents") };
        assert_eq!(target_dir(&project, &c, "skill").unwrap(), PathBuf::from("/work/app/.claude/skills"), "the other tools keep their own folders");
    }

    use super::*;
    fn home(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("antiloki-bank-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join(".claude")).unwrap(); std::fs::create_dir_all(d.join(".codex")).unwrap(); d }
    fn src_repo(d: &Path) -> PathBuf { let r = d.join("src-repo"); std::fs::create_dir_all(r.join("skills/review")).unwrap(); std::fs::write(r.join("skills/review/SKILL.md"), "---\nname: review\n---\nreview code\n").unwrap(); std::fs::create_dir_all(r.join("agents")).unwrap(); std::fs::write(r.join("agents/planner.md"), "plan\n").unwrap(); std::os::unix::fs::symlink("/etc/passwd", r.join("skills/review/evil")).unwrap(); r }

    #[test]
    fn names_and_sources_are_checked_before_anything_touches_the_disk_or_git() {
        for ok in ["code-review", "a.b_c-1", "x"] { assert!(safe_item(ok), "{ok}"); } for bad in ["", ".hidden", "-x", "a/b", "../x", "a b", "a;b", &"x".repeat(81)] { assert!(!safe_item(bad), "{bad}"); }
        for ok in ["owner/repo", "https://github.com/o/r", "git@github.com:o/r.git", "ssh://git@host/o/r"] { assert!(safe_source(ok, false), "{ok}"); }
        for bad in ["", "-oProxy", "/etc/x", "~/x", "o/r/extra", "o/../r", "o/ r", "https://x/y;rm", "file:///tmp/x", ".o/r", "a"] { assert!(!safe_source(bad, false), "{bad}"); } assert!(safe_source("file:///tmp/x", true));
        assert_eq!(git_url("o/r"), "https://github.com/o/r.git"); assert_eq!(git_url("git@h:o/r"), "git@h:o/r");
    }
    #[test]
    fn a_new_skill_is_in_the_bank_and_one_sync_links_it_into_every_tool_that_is_here_and_only_those() {
        let h = home("new"); let s = Scope::global(&h); new_skill(&s, "my-skill", "Use for tests").unwrap(); assert!(new_skill(&s, "my-skill", "").is_err());
        assert_eq!(items(&s).iter().map(|i| (i.kind, i.name.as_str())).collect::<Vec<_>>(), vec![("skill", "my-skill")]);
        let p = plan(&s, &h); assert_eq!(p.iter().map(|x| (x.0.as_str(), x.2.as_str())).collect::<Vec<_>>(), vec![("claude", "add"), ("codex", "add")], "Claude and Codex are here; Gemini, Cursor and OpenCode are not");
        let (n, skipped) = sync(&s, &h).unwrap(); assert_eq!((n, skipped.len()), (2, 0));
        assert_eq!(std::fs::read_link(h.join(".claude/skills/my-skill")).unwrap(), s.skills_dir().join("my-skill")); assert!(h.join(".codex/skills/my-skill/SKILL.md").is_file(), "through the link, the one file");
        assert!(!h.join(".gemini").exists(), "a tool that is not here is not made a folder");
        assert_eq!(sync(&s, &h).unwrap().0, 0, "a second sync changes nothing"); std::fs::write(s.skills_dir().join("my-skill/SKILL.md"), "edited once").unwrap(); assert_eq!(std::fs::read_to_string(h.join(".codex/skills/my-skill/SKILL.md")).unwrap(), "edited once", "edit once, every tool sees it");
    }
    #[test]
    fn what_a_tool_already_has_is_never_overwritten_and_a_switched_off_or_removed_skill_loses_its_links() {
        let h = home("keep"); let s = Scope::global(&h); new_skill(&s, "mine", "x").unwrap();
        std::fs::create_dir_all(h.join(".claude/skills/mine")).unwrap(); std::fs::write(h.join(".claude/skills/mine/SKILL.md"), "the person's own").unwrap();
        let (n, skipped) = sync(&s, &h).unwrap(); assert_eq!(n, 1, "only Codex got a link"); assert!(skipped[0].contains("Claude Code") && skipped[0].contains("does not touch"), "{skipped:?}"); assert_eq!(std::fs::read_to_string(h.join(".claude/skills/mine/SKILL.md")).unwrap(), "the person's own");
        set_disabled(&s, "skill", "mine", true).unwrap(); sync(&s, &h).unwrap(); assert!(!h.join(".codex/skills/mine").exists(), "switched off: the link goes"); assert!(h.join(".claude/skills/mine/SKILL.md").exists(), "the person's own stays");
        set_disabled(&s, "skill", "mine", false).unwrap(); sync(&s, &h).unwrap(); assert!(h.join(".codex/skills/mine").exists());
        remove(&s, "skill", "mine").unwrap(); sync(&s, &h).unwrap(); assert!(std::fs::symlink_metadata(h.join(".codex/skills/mine")).is_err(), "removed from the bank: the link is gone"); assert!(remove(&s, "skill", "mine").is_err()); assert!(remove(&s, "skill", "../x").is_err());
    }
    #[test]
    fn a_source_is_discovered_copied_without_links_and_never_over_what_is_there_unless_it_is_an_update_of_itself() {
        let h = home("inst"); let s = Scope::global(&h); let r = src_repo(&h);
        let (skills, agents) = discover(&r); assert_eq!((skills.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), agents.iter().map(|x| x.0.as_str()).collect::<Vec<_>>()), (vec!["review"], vec!["planner.md"]));
        let added = install_from_dir(&s, &r, "o/r", Some("abc"), false).unwrap(); assert_eq!(added, vec!["review", "planner.md"]);
        assert!(s.skills_dir().join("review/SKILL.md").is_file() && !s.skills_dir().join("review/evil").exists(), "a symlink inside a source is left behind"); assert!(s.agents_dir().join("planner.md").is_file());
        let it = items(&s); assert_eq!(it.iter().map(|i| (i.kind, i.name.as_str(), i.source.as_deref(), i.commit.as_deref())).collect::<Vec<_>>(), vec![("agent", "planner.md", Some("o/r"), Some("abc")), ("skill", "review", Some("o/r"), Some("abc"))]);
        assert!(install_from_dir(&s, &r, "o/r", Some("abc"), false).unwrap_err().contains("use update")); assert!(install_from_dir(&s, &r, "other/repo", Some("x"), true).unwrap_err().contains("not from this source"));
        std::fs::write(r.join("skills/review/SKILL.md"), "newer").unwrap(); install_from_dir(&s, &r, "o/r", Some("def"), true).unwrap(); assert_eq!(std::fs::read_to_string(s.skills_dir().join("review/SKILL.md")).unwrap(), "newer"); assert_eq!(items(&s).iter().find(|i| i.name == "review").unwrap().commit.as_deref(), Some("def"));
        let empty = h.join("empty"); std::fs::create_dir_all(&empty).unwrap(); assert!(install_from_dir(&s, &empty, "o/e", None, false).unwrap_err().contains("no skills or agents"));
    }
    #[test]
    fn the_screens_state_has_the_list_what_a_sync_would_do_and_the_tools_that_are_here() {
        let h = home("state"); let s = Scope::global(&h); new_skill(&s, "alpha", "x").unwrap(); std::fs::create_dir_all(h.join(".claude/skills/alpha")).unwrap();
        let v = read_state(&h, None, false); assert_eq!((v["installed"].clone(), v["native"].clone(), v["targets"].clone()), (json!(true), json!(true), json!(2)), "{v}");
        let g = &v["scopes"]["global"]; assert_eq!(g["list"][0]["name"], "alpha"); let t = g["diff"]["targets"].as_array().unwrap();
        assert!(t.iter().any(|x| x["name"] == "codex" && x["items"][0]["action"] == "add" && x["items"][0]["is_sync"] == true)); assert!(t.iter().any(|x| x["name"] == "claude" && x["items"][0]["is_sync"] == false), "the person's own is reported and not touched");
        assert!(v["scopes"]["project"].is_null(), "no project bank: no project scope");
        let proj = h.join("proj"); let p = Scope::project(&proj); new_skill(&p, "local-skill", "x").unwrap(); let v = read_state(&h, Some(&proj), false); assert_eq!(v["scopes"]["project"]["list"][0]["name"], "local-skill");
        assert!(perform(&h, None, &json!({ "op": "sync", "scope": "project" }))["error"].as_str().unwrap().contains("no project"));
        assert_eq!(perform(&h, None, &json!({ "op": "new", "scope": "global", "name": "beta", "description": "d" }))["ok"], true); assert!(h.join(".codex/skills/beta").exists(), "new = created and linked");
        assert_eq!(perform(&h, None, &json!({ "op": "uninstall", "scope": "global", "name": "beta" }))["ok"], true); assert!(items(&s).iter().all(|i| i.name != "beta"));
        assert_eq!(perform(&h, None, &json!({ "op": "install", "scope": "global", "source": "-oProxyCommand=x" }))["ok"], false); assert_eq!(perform(&h, None, &json!({ "op": "update", "scope": "global", "name": "alpha" }))["ok"], false, "a skill of your own has no source to update from");
    }
}
