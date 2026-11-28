//! context-library.rs — THE CONTEXT LIBRARY (operator 2026-09-19: "Claude has his own memory saved
//! somewhere on the machine, codex and opencode might have too. I want access (read-only) to those AI
//! memories at the context widget. Also all contexts from antiloki. … consolidate chunks of context and
//! also group them into context packs. Those context artefacts should be reachable across all context
//! selections of antiloki"). NAMES ARE CONTRACT: `contextLibrary`.
//!
//! One skill, five ops:
//!   sources          every CHUNK of context this box holds, grouped by who owns it — the AIs' own
//!                    memories (read-only, on disk: Claude's project memory + CLAUDE.md, the AGENTS.md
//!                    Codex/OpenCode read, Codex's memory store, OpenCode's sessions for this project)
//!                    and antiloki's (its memory layer, the session context docs, the saved papers, the
//!                    worktree packs, the installed skills)
//!   read             one chunk's body (source + id)
//!   packs            the CONTEXT PACKS the operator consolidated (context_packs table)
//!   pack.save        {id?, slug?, name, notes_md?, chunks:[{source,id,title?}]} → the pack's body is
//!                    the chunks' bodies in order, each under its provenance header; upsert by slug
//!   pack.delete      {id | slug}
//!
//! REACHABLE EVERYWHERE: a saved pack is a `context_pack` source in the notebook corpus (missions,
//! notebooks, the chat's context), a `pack` source in the pane context catalog (the bake writes it into
//! a pane's `.antiloki/context/pack/…`, `antiloki_query {source:"pack", args:{slug}}` fetches it live)
//! and a chip in every scope picker. The AI stores are NEVER written.
// QUALITY: 84/100 — real fs/sqlite reads of the three CLIs' stores; the AI stores are best-effort
// (a missing file is an empty source, never an error); pack bodies are bounded per chunk.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const CHUNK_MAX: usize = 60_000;
const PREVIEW: usize = 220;

fn s(v: Option<&Value>) -> String { match v { Some(Value::String(x)) => x.clone(), Some(Value::Null) | None => String::new(), Some(o) => o.to_string() } }
fn home() -> PathBuf { std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map(PathBuf::from).unwrap_or_default() }
fn root(db: &Connection) -> String { crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or("").to_string() }
fn preview(body: &str) -> String { let t: String = body.chars().filter(|c| *c != '\r').collect(); let one = t.lines().filter(|l| !l.trim().is_empty() && !l.starts_with("---")).take(3).collect::<Vec<_>>().join(" "); one.chars().take(PREVIEW).collect() }
fn bounded(body: String) -> String { if body.chars().count() > CHUNK_MAX { let cut: String = body.chars().take(CHUNK_MAX).collect(); format!("{cut}\n\n… (cut at {CHUNK_MAX} chars)") } else { body } }
fn mtime(p: &Path) -> String { std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| { let secs = d.as_secs() as i64; chrono_like(secs) }).unwrap_or_default() }
fn chrono_like(secs: i64) -> String {
    // ISO-ish without a chrono dependency: days since epoch → y-m-d (proleptic Gregorian), plus hh:mm
    let days = secs.div_euclid(86_400); let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, (rem % 3600) / 60)
}
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468; let era = z.div_euclid(146_097); let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
/// Claude keys a project's memory dir by its absolute path with every `/` and `.` turned into `-`.
pub fn claude_project_slug(path: &str) -> String { path.chars().map(|c| if c == '/' || c == '\\' || c == '.' || c == ':' { '-' } else { c }).collect() }

#[derive(Clone)]
struct Chunk { source: &'static str, id: String, title: String, meta: String, bytes: usize, preview: String }
fn chunk(source: &'static str, id: impl Into<String>, title: impl Into<String>, meta: impl Into<String>, body: &str) -> Chunk {
    Chunk { source, id: id.into(), title: title.into(), meta: meta.into(), bytes: body.len(), preview: preview(body) }
}
fn to_json(c: &Chunk) -> Value { json!({ "source": c.source, "id": c.id, "title": c.title, "meta": c.meta, "bytes": c.bytes, "preview": c.preview }) }

// ── the AI stores (read-only) ────────────────────────────────────────────────────────────────────
fn claude_memory_dirs(root: &str) -> Vec<(String, PathBuf)> {
    let base = home().join(".claude").join("projects");
    let want = claude_project_slug(root);
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(&base) {
        let mut names: Vec<String> = rd.flatten().filter_map(|e| e.file_name().to_str().map(String::from)).collect();
        names.sort();
        for n in names {
            let dir = base.join(&n).join("memory");
            if !dir.is_dir() { continue; }
            // this workspace first, then its boxes (the worktrees live under ~/.antiloki/worktrees), then the rest
            let rank = if n == want { 0 } else if n.contains("antiloki-worktrees") { 1 } else { 2 };
            out.push((format!("{rank}{n}"), dir));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.into_iter().map(|(k, d)| (k[1..].to_string(), d)).collect()
}
fn claude_memory(root: &str, read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    let mut out = vec![];
    for (proj, dir) in claude_memory_dirs(root) {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x == "md")).collect();
            files.sort();
            for f in files {
                let id = format!("{proj}/{}", f.file_name().and_then(|x| x.to_str()).unwrap_or(""));
                if let Some(want) = read { if want != id { continue; } return Err(std::fs::read_to_string(&f).ok().map(bounded)); }
                let body = std::fs::read_to_string(&f).unwrap_or_default();
                let title = body.lines().find_map(|l| l.strip_prefix("name: ")).or_else(|| body.lines().find_map(|l| l.strip_prefix("# "))).map(String::from).unwrap_or_else(|| f.file_name().and_then(|x| x.to_str()).unwrap_or("").to_string());
                out.push(chunk("claude.memory", id, title, format!("{} · {}", proj.trim_start_matches('-').replace("--", "/."), mtime(&f)), &body));
            }
        }
    }
    if read.is_some() { return Err(None); }
    Ok(out)
}
fn rules_files(root: &str, read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    // the instruction files every CLI reads at the door: Claude's, and AGENTS.md (Codex + OpenCode)
    let mut cands: Vec<(&'static str, &'static str, PathBuf)> = vec![
        ("claude.rules", "global CLAUDE.md", home().join(".claude").join("CLAUDE.md")),
        ("claude.rules", "CLAUDE.md", Path::new(root).join("CLAUDE.md")),
        ("claude.rules", ".claude/CLAUDE.md", Path::new(root).join(".claude").join("CLAUDE.md")),
        ("agents.rules", "AGENTS.md", Path::new(root).join("AGENTS.md")),
        ("agents.rules", "global AGENTS.md (codex)", home().join(".codex").join("AGENTS.md")),
        ("agents.rules", "global AGENTS.md (opencode)", home().join(".config").join("opencode").join("AGENTS.md")),
    ];
    cands.retain(|(_, _, p)| p.is_file());
    let mut out = vec![];
    for (src, label, p) in cands {
        let id = p.to_string_lossy().into_owned();
        if let Some(want) = read { if want == id { return Err(std::fs::read_to_string(&p).ok().map(bounded)); } continue; }
        let body = std::fs::read_to_string(&p).unwrap_or_default();
        out.push(chunk(src, id, label, mtime(&p), &body));
    }
    if read.is_some() { return Err(None); }
    Ok(out)
}
fn codex_memory(read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    let p = home().join(".codex").join("memories_1.sqlite");
    if !p.is_file() { return if read.is_some() { Err(None) } else { Ok(vec![]) }; }
    let Ok(c) = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return if read.is_some() { Err(None) } else { Ok(vec![]) }; };
    let mut out = vec![];
    if let Ok(mut st) = c.prepare("SELECT thread_id, rollout_slug, raw_memory, rollout_summary, source_updated_at FROM stage1_outputs ORDER BY source_updated_at DESC LIMIT 200") {
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?))).ok();
        for row in rows.into_iter().flatten().flatten() {
            let (thread, slug, raw, summary, at) = row;
            let body = format!("{raw}\n\n---\n{summary}");
            if let Some(want) = read { if want == thread { return Err(Some(bounded(body))); } continue; }
            out.push(chunk("codex.memory", thread.clone(), slug.unwrap_or_else(|| thread.chars().take(12).collect()), chrono_like(if at > 10_000_000_000 { at / 1000 } else { at }), &body));
        }
    }
    if read.is_some() { return Err(None); }
    Ok(out)
}
fn opencode_sessions(root: &str, read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    let p = home().join(".local").join("share").join("opencode").join("opencode.db");
    if !p.is_file() { return if read.is_some() { Err(None) } else { Ok(vec![]) }; }
    let Ok(c) = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return if read.is_some() { Err(None) } else { Ok(vec![]) }; };
    let mut out = vec![];
    let sql = "SELECT s.id, s.title, s.directory, s.time_updated, p.worktree FROM session s JOIN project p ON p.id = s.project_id ORDER BY s.time_updated DESC LIMIT 400";
    if let Ok(mut st) = c.prepare(sql) {
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?))).ok();
        for row in rows.into_iter().flatten().flatten() {
            let (id, title, dir, at, wt) = row;
            // this project = the workspace root or one of its boxes
            let ours = wt.starts_with(root) || dir.starts_with(root) || wt.contains("/.antiloki/worktrees/") || dir.contains("/.antiloki/worktrees/");
            if !ours { continue; }
            if let Some(want) = read {
                if want != id { continue; }
                // the session's text parts, in order (bounded)
                let mut body = format!("# {title}\n{dir}\n\n");
                if let Ok(mut ps) = c.prepare("SELECT data FROM part WHERE session_id = ? ORDER BY time_created ASC LIMIT 400") {
                    let parts = ps.query_map([&id], |r| r.get::<_, String>(0)).ok();
                    for d in parts.into_iter().flatten().flatten() {
                        if let Ok(v) = serde_json::from_str::<Value>(&d) { if v.get("type").and_then(Value::as_str) == Some("text") { if let Some(t) = v.get("text").and_then(Value::as_str) { body.push_str(t); body.push_str("\n\n"); } } }
                        if body.len() > CHUNK_MAX { break; }
                    }
                }
                return Err(Some(bounded(body)));
            }
            out.push(chunk("opencode.sessions", id, title, format!("{} · {}", chrono_like(if at > 10_000_000_000 { at / 1000 } else { at }), dir.rsplit('/').next().unwrap_or("")), ""));
        }
    }
    if read.is_some() { return Err(None); }
    Ok(out)
}

// ── antiloki's own ───────────────────────────────────────────────────────────────────────────────
fn antiloki_memory(db: &Connection, read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    let rows = crate::entities_memory_list::memoryList(&json!({ "activeOnly": false, "limit": 300 }), db).ok().and_then(|v| v.get("memories").cloned().or_else(|| v.get("items").cloned()).or(Some(v))).and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut out = vec![];
    for m in rows {
        let id = s(m.get("id"));
        let body = format!("# {}\n\n**Situation:** {}\n\n**Guidance:** {}{}", s(m.get("title")), s(m.get("situation")), s(m.get("guidance")), m.get("rationale").and_then(Value::as_str).filter(|r| !r.is_empty()).map(|r| format!("\n\n**Why:** {r}")).unwrap_or_default());
        if let Some(want) = read { if want == id { return Err(Some(bounded(body))); } continue; }
        out.push(chunk("antiloki.memory", id, s(m.get("title")), format!("{} · {}", s(m.get("type")), s(m.get("created_at")).chars().take(16).collect::<String>()), &body));
    }
    if read.is_some() { return Err(None); }
    Ok(out)
}
fn blackboard_docs(db: &Connection, read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    let rows = crate::db_json::query_rows(db, "SELECT b.id, b.session_id, b.body_md, b.created_at, s.name AS session_name FROM cockpit_blackboard b LEFT JOIN cockpit_sessions s ON s.id = b.session_id WHERE b.kind = 'context' ORDER BY b.id DESC LIMIT 300", &[]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut out = vec![];
    for r in rows {
        let id = s(r.get("id")); let body = s(r.get("body_md"));
        if let Some(want) = read { if want == id { return Err(Some(bounded(body))); } continue; }
        let title = body.lines().find(|l| !l.trim().is_empty()).unwrap_or("context doc").chars().take(90).collect::<String>();
        out.push(chunk("antiloki.blackboard", id, title, format!("{} · {}", s(r.get("session_name")), s(r.get("created_at")).chars().take(16).collect::<String>()), &body));
    }
    if read.is_some() { return Err(None); }
    Ok(out)
}
fn papers(db: &Connection, read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    if let Some(want) = read {
        let r = crate::constitution_constitution_paper::constitutionPaper(&json!({ "op": "get", "id": want.parse::<i64>().unwrap_or(0) }), db).ok();
        return Err(r.and_then(|v| v.get("paper").and_then(|p| p.get("markdown")).and_then(Value::as_str).map(|m| bounded(m.to_string()))));
    }
    let list = crate::constitution_constitution_paper::constitutionPaper(&json!({ "op": "list", "limit": 100 }), db).ok().and_then(|v| v.get("papers").cloned()).and_then(|v| v.as_array().cloned()).unwrap_or_default();
    Ok(list.iter().map(|p| chunk("antiloki.papers", s(p.get("id")), s(p.get("title")), format!("{} · {}", s(p.get("label")), s(p.get("created_at")).chars().take(16).collect::<String>()), &s(p.get("discovery")))).collect())
}
fn worktree_packs(read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    let base = home().join(".antiloki").join("worktrees");
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(&base) {
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect(); dirs.sort();
        for d in dirs {
            let idx = d.join(".antiloki").join("context").join("INDEX.md");
            if !idx.is_file() { continue; }
            let id = d.file_name().and_then(|x| x.to_str()).unwrap_or("").to_string();
            if let Some(want) = read { if want == id { return Err(std::fs::read_to_string(&idx).ok().map(bounded)); } continue; }
            let body = std::fs::read_to_string(&idx).unwrap_or_default();
            out.push(chunk("antiloki.packs", id.clone(), format!("{id} · context pack"), mtime(&idx), &body));
        }
    }
    if read.is_some() { return Err(None); }
    Ok(out)
}
fn skills(root: &str, read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    let mut out = vec![];
    for base in [Path::new(root).join(".agents").join("skills"), Path::new(root).join(".claude").join("skills")] {
        if let Ok(rd) = std::fs::read_dir(&base) {
            let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect(); dirs.sort();
            for d in dirs {
                let f = d.join("SKILL.md");
                let id = f.to_string_lossy().into_owned();
                if let Some(want) = read { if want == id { return Err(std::fs::read_to_string(&f).ok().map(bounded)); } continue; }
                let body = std::fs::read_to_string(&f).unwrap_or_default();
                out.push(chunk("antiloki.skills", id, d.file_name().and_then(|x| x.to_str()).unwrap_or("").to_string(), mtime(&f), &body));
            }
        }
    }
    if read.is_some() { return Err(None); }
    Ok(out)
}

pub const SOURCES: &[(&str, &str, &str, &str)] = &[
    // key · owner · label · note
    ("claude.memory", "claude", "Claude's memory", "~/.claude/projects/<project>/memory — the notes Claude Code keeps per project (read-only)"),
    ("claude.rules", "claude", "CLAUDE.md", "the instruction files Claude reads at the door (read-only)"),
    ("codex.memory", "codex", "Codex's memory", "~/.codex/memories — what Codex distilled from its threads (read-only)"),
    ("agents.rules", "codex", "AGENTS.md", "the instruction file Codex and OpenCode read at the door (read-only)"),
    ("opencode.sessions", "opencode", "OpenCode's sessions", "~/.local/share/opencode — this project's sessions, their text (read-only)"),
    ("antiloki.memory", "antiloki", "antiloki memory", "the memory layer: lessons, corrections, patterns"),
    ("antiloki.blackboard", "antiloki", "session context docs", "the shared context docs every agent of a session reads"),
    ("antiloki.papers", "antiloki", "papers", "the Ultra review studies"),
    ("antiloki.packs", "antiloki", "worktree packs", "the context pack baked into each agent box (INDEX.md)"),
    ("antiloki.skills", "antiloki", "skills", "the installed agent skills"),
];

fn gather(db: &Connection, root: &str, source: &str, read: Option<&str>) -> Result<Vec<Chunk>, Option<String>> {
    match source {
        "claude.memory" => claude_memory(root, read),
        "claude.rules" | "agents.rules" => rules_files(root, read).map(|v| v.into_iter().filter(|c| c.source == source).collect()),
        "codex.memory" => codex_memory(read),
        "opencode.sessions" => opencode_sessions(root, read),
        "antiloki.memory" => antiloki_memory(db, read),
        "antiloki.blackboard" => blackboard_docs(db, read),
        "antiloki.papers" => papers(db, read),
        "antiloki.packs" => worktree_packs(read),
        "antiloki.skills" => skills(root, read),
        "context.pack" => {
            if let Some(want) = read { return Err(pack_body(db, want)); }
            Ok(pack_rows(db).iter().map(|p| chunk("context.pack", s(p.get("slug")), s(p.get("name")), format!("{} chunks · {}", p.get("chunks").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0), s(p.get("updated_at")).chars().take(16).collect::<String>()), &s(p.get("notes_md")))).collect())
        }
        _ => Ok(vec![]),
    }
}
/// A chunk's body — `Some` when found. Used by the corpus, the catalog and the pack builder.
pub fn read_chunk(db: &Connection, source: &str, id: &str) -> Option<String> {
    let root = root(db);
    match gather(db, &root, source, Some(id)) { Err(body) => body, Ok(_) => None }
}

// ── the packs ────────────────────────────────────────────────────────────────────────────────────
fn ensure_table(db: &Connection) {
    let _ = db.execute_batch("CREATE TABLE IF NOT EXISTS context_packs (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, name TEXT NOT NULL, notes_md TEXT NOT NULL DEFAULT '', chunks_json TEXT NOT NULL DEFAULT '[]', body_md TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL, updated_at TEXT NOT NULL)");
}
fn pack_rows(db: &Connection) -> Vec<Value> {
    ensure_table(db);
    crate::db_json::query_rows(db, "SELECT id, slug, name, notes_md, chunks_json, LENGTH(body_md) AS bytes, created_at, updated_at FROM context_packs ORDER BY updated_at DESC", &[]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
        .into_iter().map(|mut r| { let chunks = serde_json::from_str::<Value>(&s(r.get("chunks_json"))).unwrap_or(json!([])); if let Some(o) = r.as_object_mut() { o.remove("chunks_json"); o.insert("chunks".into(), chunks); } r }).collect()
}
pub fn pack_body(db: &Connection, slug: &str) -> Option<String> {
    ensure_table(db);
    crate::db_json::query_rows(db, "SELECT body_md FROM context_packs WHERE slug = ? OR id = ?", &[json!(slug), json!(slug.parse::<i64>().unwrap_or(-1))]).ok()
        .and_then(|v| v.as_array().and_then(|a| a.first()).and_then(|r| r.get("body_md")).and_then(Value::as_str).map(String::from))
}
pub fn slugify(name: &str) -> String {
    let mut out = String::new(); let mut dash = false;
    for c in name.chars() { if c.is_alphanumeric() { out.push(c.to_ascii_lowercase()); dash = false; } else if !dash && !out.is_empty() { out.push('-'); dash = true; } }
    let t = out.trim_matches('-').to_string(); if t.is_empty() { "pack".into() } else { t.chars().take(60).collect() }
}
/// The pack's body: the notes, then every chunk under a provenance header — the artefact every
/// context selection hands to an agent. Pure over the bodies it is given.
pub fn compose(name: &str, notes: &str, parts: &[(String, String, String, String)]) -> String {
    let mut md = format!("# Context pack · {name}\n\n");
    if !notes.trim().is_empty() { md.push_str(notes.trim()); md.push_str("\n\n"); }
    for (source, id, title, body) in parts {
        md.push_str(&format!("---\n\n## {title}\n\n<!-- provenance: {source} · {id} -->\n\n{}\n\n", body.trim()));
    }
    md
}

pub fn contextLibrary(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = { let o = s(input.get("op")); if o.is_empty() { "sources".to_string() } else { o } };
    let root = root(db);
    match op.as_str() {
        "sources" => {
            let only = s(input.get("source"));
            let mut groups: Vec<Value> = vec![];
            for (key, owner, label, note) in SOURCES.iter().chain([("context.pack", "antiloki", "context packs", "the packs consolidated here — reachable from every context selection")].iter()) {
                if !only.is_empty() && only != *key { continue; }
                let chunks = gather(db, &root, key, None).unwrap_or_default();
                groups.push(json!({ "key": key, "owner": owner, "label": label, "note": note, "read_only": *owner != "antiloki", "chunks": chunks.iter().map(to_json).collect::<Vec<_>>() }));
            }
            Ok(json!({ "ok": true, "root": root, "sources": groups }))
        }
        "read" => {
            let source = s(input.get("source")); let id = s(input.get("id"));
            match read_chunk(db, &source, &id) { Some(body) => Ok(json!({ "ok": true, "source": source, "id": id, "body": body })), None => Ok(json!({ "ok": false, "error": format!("no chunk {id} in {source}") })) }
        }
        "packs" => Ok(json!({ "ok": true, "packs": pack_rows(db) })),
        "pack.get" => {
            let slug = s(input.get("slug"));
            let row = pack_rows(db).into_iter().find(|p| s(p.get("slug")) == slug || s(p.get("id")) == slug);
            match row { Some(mut r) => { r["body_md"] = json!(pack_body(db, &slug).unwrap_or_default()); Ok(json!({ "ok": true, "pack": r })) } None => Ok(json!({ "ok": false, "error": format!("no pack {slug}") })) }
        }
        "pack.save" => {
            ensure_table(db);
            let name = s(input.get("name")).trim().to_string();
            if name.is_empty() { return Err("context-library: pack.save needs a name".into()); }
            let slug = { let sl = s(input.get("slug")); if sl.is_empty() { slugify(&name) } else { slugify(&sl) } };
            let notes = s(input.get("notes_md"));
            let chunks: Vec<Value> = input.get("chunks").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut parts = vec![]; let mut kept = vec![];
            for c in &chunks {
                let source = s(c.get("source")); let id = s(c.get("id"));
                // a pasted chunk carries its own body; a referenced one is read now (the pack is a snapshot)
                let body = c.get("body").and_then(Value::as_str).map(String::from).or_else(|| read_chunk(db, &source, &id));
                let Some(body) = body else { continue };
                let title = { let t = s(c.get("title")); if t.is_empty() { format!("{source} · {id}") } else { t } };
                kept.push(json!({ "source": source, "id": id, "title": title, "bytes": body.len() }));
                parts.push((source, id, title, body));
            }
            let body_md = compose(&name, &notes, &parts);
            let now = crate::clock::now_iso(db);
            db.execute("INSERT INTO context_packs (slug, name, notes_md, chunks_json, body_md, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) ON CONFLICT(slug) DO UPDATE SET name = excluded.name, notes_md = excluded.notes_md, chunks_json = excluded.chunks_json, body_md = excluded.body_md, updated_at = excluded.updated_at",
                rusqlite::params![slug, name, notes, serde_json::to_string(&kept).unwrap_or_else(|_| "[]".into()), body_md, now]).map_err(|e| format!("context-library: {e}"))?;
            let row = pack_rows(db).into_iter().find(|p| s(p.get("slug")) == slug).unwrap_or(Value::Null);
            Ok(json!({ "ok": true, "pack": row, "chars": body_md.chars().count() }))
        }
        "pack.delete" => {
            ensure_table(db);
            let slug = s(input.get("slug")); let id = input.get("id").and_then(Value::as_i64).unwrap_or(-1);
            let n = db.execute("DELETE FROM context_packs WHERE slug = ?1 OR id = ?2", rusqlite::params![slug, id]).map_err(|e| e.to_string())?;
            Ok(json!({ "ok": n > 0, "deleted": n }))
        }
        other => Err(format!("context-library: unknown op `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude_keys_a_project_by_its_path_with_dashes() {
        assert_eq!(claude_project_slug("/home/user/dev/antiloki"), "-home-user-dev-antiloki");
        assert_eq!(claude_project_slug("/home/user/.antiloki/worktrees/cockpit-1-1"), "-home-user--antiloki-worktrees-cockpit-1-1");
    }
    #[test]
    fn slugs_and_composition() {
        assert_eq!(slugify("My Pack · Q3 rules!"), "my-pack-q3-rules");
        assert_eq!(slugify("   "), "pack");
        let md = compose("p", "why", &[("claude.memory".into(), "x/a.md".into(), "A".into(), "body a".into())]);
        assert!(md.starts_with("# Context pack · p\n\nwhy\n\n---\n\n## A\n\n<!-- provenance: claude.memory · x/a.md -->\n\nbody a"));
        assert_eq!(chrono_like(0), "1970-01-01 00:00"); assert_eq!(chrono_like(1_789_852_856), "2026-09-19 21:20");
    }
    #[test]
    fn a_pack_is_saved_from_pasted_chunks_listed_read_back_and_deleted() {
        let c = Connection::open_in_memory().unwrap();
        // the skill's own table; the root lookup needs the workspaces table the engine has
        let _ = c.execute_batch("CREATE TABLE workspaces (id INTEGER PRIMARY KEY, root_path TEXT, is_active INTEGER)");
        let r = contextLibrary(&json!({ "op": "pack.save", "name": "Team rules", "notes_md": "for every agent", "chunks": [{ "source": "paste", "id": "1", "title": "Rule one", "body": "never push to main" }, { "source": "nope", "id": "missing" }] }), &c).unwrap();
        assert_eq!(r["ok"], json!(true)); assert_eq!(r["pack"]["slug"], json!("team-rules")); assert_eq!(r["pack"]["chunks"].as_array().unwrap().len(), 1, "the unreadable chunk is dropped");
        let body = pack_body(&c, "team-rules").unwrap();
        assert!(body.contains("## Rule one") && body.contains("never push to main") && body.contains("provenance: paste · 1"));
        let list = contextLibrary(&json!({ "op": "packs" }), &c).unwrap();
        assert_eq!(list["packs"].as_array().unwrap().len(), 1);
        let again = contextLibrary(&json!({ "op": "pack.save", "slug": "team-rules", "name": "Team rules v2", "chunks": [] }), &c).unwrap();
        assert_eq!(again["pack"]["name"], json!("Team rules v2"));
        assert_eq!(contextLibrary(&json!({ "op": "packs" }), &c).unwrap()["packs"].as_array().unwrap().len(), 1, "upsert by slug");
        assert_eq!(contextLibrary(&json!({ "op": "pack.delete", "slug": "team-rules" }), &c).unwrap()["ok"], json!(true));
        assert!(pack_body(&c, "team-rules").is_none());
        assert!(contextLibrary(&json!({ "op": "pack.save", "name": "" }), &c).is_err());
    }
}
