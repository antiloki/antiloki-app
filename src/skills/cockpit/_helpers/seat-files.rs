//! seat-files.rs — an agent's craft (skills, specialists, laws) in the place ITS SEAT reads it, inside ITS worktree. NO JS TWIN.
//!
//! WHY (operator, X-ray/agents audit 2026-10-01: "skills and specialists are not materialized for Cursor agents"): the five
//! seats are five different CLIs and each one loads its instructions from its own files. antiloki wrote ONE layout — the
//! Claude Code one (`.claude/skills/<slug>/SKILL.md`, `CLAUDE.md`) plus a vendor-neutral `.agents/skills` and an `AGENTS.md`
//! index — and called it done. That reaches Claude, and Codex/OpenCode through the AGENTS.md index; it left a Cursor, a
//! Gemini and a Grok agent hired with skills and specialists working without them, and nothing said so.
//!
//! WHERE EACH SEAT READS (verified 2026-10-01 against the installed CLIs — Cursor's bundled loader, Antigravity's built-in
//! customization guide, Grok's `~/.grok/docs/user-guide`; not assumed from the vendor names):
//!
//! | seat | skills (a `<slug>/SKILL.md` per skill) | rules / instructions |
//! |---|---|---|
//! | Claude Code | `.claude/skills` | `CLAUDE.md`, `AGENTS.md` |
//! | Codex | `.agents/skills` | `AGENTS.md` |
//! | OpenCode | `.agents/skills`, `.claude/skills` | `AGENTS.md` |
//! | Cursor | `.cursor/skills` (first-party; `.agents/skills` and `.claude/skills` only while its third-party-config setting is on) | `.cursor/rules/*.mdc` (`alwaysApply`), `AGENTS.md` |
//! | Gemini (Antigravity) | `.agents/skills` | `AGENTS.md`, `GEMINI.md`, `.agents/rules/*.md` |
//! | Grok | `.grok/skills` (also `.agents/skills`, `.claude/skills`) | `AGENTS.md`, `.grok/rules/*.md` |
//!
//! So a skill is written where its seat reads it NATIVELY and always into `.agents/skills` too (the neutral place the
//! AGENTS.md index points at), and a seat with a rules directory gets ONE file of its own, `antiloki.<ext>`, that carries what
//! the brief in AGENTS.md cannot: the skills with the path THIS seat finds them at, the review specialists as lenses to apply
//! to its own diff, and where the laws are. It is deliberately short — a Cursor `alwaysApply` rule rides in every prompt.
//!
//! Everything is written into the pane's worktree and nowhere else; a file is overwritten only when it is ours (the rules file
//! carries `RULES_MARK`, a skill dir carries the `.antiloki-materialized` marker), so the project's own `.cursor/skills/*` or
//! `.cursor/rules/antiloki.mdc` is never touched. A pane that was hired with no skills and no specialists gets nothing new.
//!
//! NOT DONE, on purpose: the live MCP tools (`antiloki_specialist` runs an isolated review). Cursor, Grok and Gemini have no
//! MCP registration in antiloki yet — `cursor-agent` wants `.cursor/mcp.json` plus `--approve-mcps`, Antigravity `mcp_config.json`
//! — so the specialists reach those seats as lenses in the rules file, not as a tool they can call.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const RULES_MARK: &str = "<!-- ANTILOKI-SEAT-RULES -->";

/// Every directory any seat reads skills from — a dir of ours left in one of them by a previous seat is cleaned up.
pub const ALL_SKILL_DIRS: &[&str] = &[".agents/skills", ".claude/skills", ".cursor/skills", ".grok/skills"];

/// ONE LINE, NO CONTROL CHARACTERS (audit 2026-10-01, F-16): a skill's name and description come from the library (an agent can author a row) and land in a front matter and in a bullet list;
/// a newline in `name` ended the line and let the rest of it be a front-matter key or a list item of its own. Every control character (and the Unicode line/paragraph separators) becomes a space. Pure - tested.
pub fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() || c == '\u{2028}' || c == '\u{2029}' || c == '\u{85}' { ' ' } else { c }).collect::<String>().trim().to_string()
}

/// Write `bytes` to `target`, which must end up INSIDE `root` (audit 2026-10-01, F-16: `fs::write` follows symlinks, and a worktree is a checkout of a repository somebody else wrote: a
/// `.claude/skills` or `AGENTS.md` that is a symlink to `~/.ssh/authorized_keys` was written through). Rules:
///  - every directory above the target is resolved (symlinks and all) and must stay under the resolved root - checked on the deepest part that exists BEFORE anything is created, and again after;
///  - a target that is itself a symlink is followed only when it resolves to a file under the root (a repo may link `CLAUDE.md` to `AGENTS.md`); a dangling link, or one that leaves, is refused;
///  - the bytes go to a temp file beside the final path and are renamed over it, so a reader never sees half a file and a crash leaves the old one.
pub fn write_contained(root: &Path, target: &Path, bytes: &[u8]) -> Result<(), String> {
    let croot = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let parent = target.parent().ok_or_else(|| format!("{}: no parent", target.display()))?;
    let name = target.file_name().ok_or_else(|| format!("{}: no file name", target.display()))?;
    // the deepest ancestor that exists, resolved: it must already be inside the root, or creating the missing part below it would happen outside
    let mut deepest = parent;
    while !deepest.exists() { deepest = match deepest.parent() { Some(p) if !p.as_os_str().is_empty() => p, _ => break } }
    if deepest.exists() && !std::fs::canonicalize(deepest).map_or(false, |c| c.starts_with(&croot)) {
        return Err(format!("{}: refused - it resolves outside the workspace", parent.display()));
    }
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let cparent = std::fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    if !cparent.starts_with(&croot) { return Err(format!("{}: refused - it resolves outside the workspace", parent.display())) }
    let mut dest = cparent.join(name);
    if let Ok(md) = std::fs::symlink_metadata(&dest) {
        if md.file_type().is_symlink() {
            match std::fs::canonicalize(&dest) {
                Ok(c) if c.starts_with(&croot) && c.is_file() => dest = c,
                _ => return Err(format!("{}: refused - it is a symlink that does not resolve to a file in the workspace", target.display())),
            }
        } else if md.is_dir() {
            return Err(format!("{}: refused - it is a directory", target.display()));
        }
    }
    let tmp = dest.with_file_name(format!(".{}.antiloki-tmp-{}", dest.file_name().and_then(|n| n.to_str()).unwrap_or("file"), std::process::id()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &dest).map_err(|e| { let _ = std::fs::remove_file(&tmp); format!("{}: {e}", dest.display()) })
}

/// Where a seat loads skills from, native place first. A kind that is not a known seat keeps the layout every pane had before
/// 2026-10-01 (`.agents/skills` + `.claude/skills`). Pure — tested.
pub fn skill_dirs(kind: &str) -> &'static [&'static str] {
    match kind {
        "claude-code" => &[".claude/skills", ".agents/skills"],
        "cursor" => &[".cursor/skills", ".agents/skills"],
        "grok" => &[".grok/skills", ".agents/skills"],
        "gemini" | "codex" => &[".agents/skills"],
        _ => &[".agents/skills", ".claude/skills"],
    }
}

/// The seat's own rules file for what the brief does not carry: relative path and the front matter it needs. `None` for the
/// seats whose instruction files (`CLAUDE.md`, `AGENTS.md`) already hold the brief and the skills index. Pure — tested.
pub fn rules_file(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "cursor" => Some((".cursor/rules/antiloki.mdc", "---\ndescription: antiloki - the skills, review specialists and laws this agent was hired with\nalwaysApply: true\n---\n")),
        "gemini" => Some((".agents/rules/antiloki.md", "")),
        "grok" => Some((".grok/rules/antiloki.md", "")),
        _ => None,
    }
}

/// The files of ours a seat may have in `.git/info/exclude` terms: they must never reach a PR.
pub const EXCLUDED_RULES_FILES: &[&str] = &[".cursor/rules/antiloki.mdc", ".agents/rules/antiloki.md", ".grok/rules/antiloki.md"];

/// One skill as the rules file lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillRef { pub slug: String, pub name: String, pub description: String }

/// The rules file body. `native_dir` is the skills directory THIS seat loads from. Pure — tested.
pub fn render_rules(native_dir: &str, skills: &[SkillRef], specialists: &[(String, String)], laws_path: Option<&str>) -> String {
    let mut out: Vec<String> = vec![
        RULES_MARK.to_string(),
        "# antiloki - what this agent was hired with (generated; rewritten on every start)".to_string(),
    ];
    if !skills.is_empty() {
        out.push(String::new());
        out.push("## Skills - open a skill's SKILL.md when its description matches the task".to_string());
        for s in skills { out.push(format!("- **{}** - {} -> `{native_dir}/{}/SKILL.md`", one_line(&s.name), one_line(&s.description), one_line(&s.slug))); }
    }
    if !specialists.is_empty() {
        out.push(String::new());
        out.push("## Review specialists - apply each lens to your own diff before you report done".to_string());
        for (id, what) in specialists { out.push(format!("- `{}` - {}", one_line(id), one_line(what))); }
    }
    if let Some(p) = laws_path {
        out.push(String::new());
        out.push("## Laws".to_string());
        out.push(format!("The ratified laws your edits are judged against are in `{p}`. Read them before you edit; a change that breaks one is sent back."));
    }
    out.join("\n") + "\n"
}

fn s(v: Option<&Value>) -> String { match v { Some(Value::String(x)) => x.clone(), Some(Value::Null) | None => String::new(), Some(o) => o.to_string() } }

/// 2026-10-13: tracked files in a main checkout (`.git` is a directory) are not rewritten or deleted. A linked worktree (`.git` is a file) stays writable.
fn hands_off(wt: &Path, rel: &str) -> bool {
    wt.join(".git").is_dir() && crate::proc::cmd("git").args(["ls-files", "--error-unmatch", rel]).current_dir(wt).output().ok().filter(|o| o.status.success()).is_some()
}

/// Write (or take away) the seat's rules file for `pane` in `wt`. Returns what happened, for the caller's log.
///   - hired with skills or specialists -> the file is written (only over a file that is ours, or none);
///   - hired with neither -> a file of ours left from an earlier start is removed.
/// The skills themselves are written by `skillLibraryMaterialize`, which the caller has already run. Never fails the bake.
pub fn write_rules(db: &Connection, pane: &Value, wt: &Path, skills: &[SkillRef]) -> Value {
    let kind = s(pane.get("pane_kind"));
    let ours = |p: &Path| std::fs::read_to_string(p).map_or(true, |t| t.contains(RULES_MARK));
    // a rules file of ours left by ANOTHER seat (the pane moved from Cursor to Grok) is taken away: it would be read by whoever finds it.
    // 2026-10-13: a tracked copy in a main checkout is the project's — not deleted.
    for other in EXCLUDED_RULES_FILES.iter().filter(|r| Some(**r) != rules_file(&kind).map(|f| f.0)) {
        if hands_off(wt, other) { continue; }
        let p = wt.join(other);
        if p.is_file() && std::fs::read_to_string(&p).map_or(false, |t| t.contains(RULES_MARK)) { let _ = std::fs::remove_file(&p); }
    }
    let Some((rel, front)) = rules_file(&kind) else { return json!({ "rules": Value::Null }) };
    if hands_off(wt, rel) { return json!({ "rules": Value::Null, "skipped": format!("{rel} is tracked in this checkout") }); }
    let path: PathBuf = wt.join(rel);
    let settings: Value = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    // specialists only when the agent was hired with an allow-list: with none, `pane_specialists` is every ready lens, which is not "its" craft
    let hired_specialists = settings.get("specialists").and_then(Value::as_array).map_or(false, |a| !a.is_empty());
    let specialists: Vec<(String, String)> = if hired_specialists {
        crate::cockpit_cockpit_pane_context_bake::pane_specialists(db, pane).iter()
            .map(|a| (s(a.get("id")), format!("{}{}", s(a.get("title")), { let d = s(a.get("description")); if d.is_empty() { String::new() } else { format!(": {d}") } }))).collect()
    } else { vec![] };
    if skills.is_empty() && specialists.is_empty() {
        if path.is_file() && ours(&path) { let _ = std::fs::remove_file(&path); return json!({ "rules": Value::Null, "removed": rel }); }
        return json!({ "rules": Value::Null });
    }
    if path.is_file() && !ours(&path) { return json!({ "rules": Value::Null, "skipped": format!("{rel} is the project's own file") }); }
    let native = skill_dirs(&kind)[0];
    let laws = wt.join(crate::cockpit_cockpit_pane_context_bake::CONTEXT_DIR).join("laws.md").is_file().then(|| format!("{}/laws.md", crate::cockpit_cockpit_pane_context_bake::CONTEXT_DIR));
    let body = render_rules(native, skills, &specialists, laws.as_deref());
    let content = format!("{front}{body}");
    // through `write_contained` (audit 2026-10-01, F-16): never through a symlink that leaves the worktree, and by temp file + rename
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
        if let Err(e) = write_contained(wt, &path, content.as_bytes()) { return json!({ "rules": Value::Null, "skipped": e }); }
    }
    // 2026-10-13: a newly written rules file in a main checkout stays out of git status.
    if wt.join(".git").is_dir() { crate::cockpit_cockpit_pane_context_bake::git_exclude_line(wt, rel); }
    json!({ "rules": rel, "skills": skills.len(), "specialists": specialists.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(slug: &str, name: &str, d: &str) -> SkillRef { SkillRef { slug: slug.into(), name: name.into(), description: d.into() } }

    #[test]
    fn each_seat_reads_skills_from_its_own_directory_and_the_neutral_one() {
        assert_eq!(skill_dirs("cursor"), [".cursor/skills", ".agents/skills"]);
        assert_eq!(skill_dirs("grok"), [".grok/skills", ".agents/skills"]);
        assert_eq!(skill_dirs("gemini"), [".agents/skills"]);
        assert_eq!(skill_dirs("codex"), [".agents/skills"]);
        assert_eq!(skill_dirs("claude-code"), [".claude/skills", ".agents/skills"]);
        // a pane of unknown kind keeps the layout every pane had before the seats were told apart
        assert_eq!(skill_dirs(""), [".agents/skills", ".claude/skills"]);
        assert_eq!(skill_dirs("agent"), [".agents/skills", ".claude/skills"]);
        for d in ["cursor", "grok", "gemini", "codex", "claude-code", ""].iter().flat_map(|k| skill_dirs(k).iter()) {
            assert!(ALL_SKILL_DIRS.contains(d), "{d} must be cleaned when the pane moves to another seat");
        }
    }

    #[test]
    fn only_the_seats_with_a_rules_directory_get_a_rules_file_of_their_own() {
        let (cursor, front) = rules_file("cursor").unwrap();
        assert_eq!(cursor, ".cursor/rules/antiloki.mdc");
        assert!(front.starts_with("---\n") && front.contains("alwaysApply: true") && front.ends_with("---\n"), "Cursor only loads a .mdc rule with front matter: {front}");
        assert_eq!(rules_file("gemini"), Some((".agents/rules/antiloki.md", "")));
        assert_eq!(rules_file("grok"), Some((".grok/rules/antiloki.md", "")));
        for none in ["claude-code", "codex", "opencode", "opencode-direct", ""] { assert_eq!(rules_file(none), None, "{none} reads CLAUDE.md / AGENTS.md, which already carry the brief"); }
        for (r, _) in ["cursor", "gemini", "grok"].iter().filter_map(|k| rules_file(k)) { assert!(EXCLUDED_RULES_FILES.contains(&r), "{r} must never reach a PR"); }
    }

    #[test]
    fn the_rules_file_lists_skills_with_the_path_this_seat_finds_them_at() {
        let md = render_rules(".cursor/skills", &[skill("tdd", "TDD", "write the test first\nthen the code")], &[("axis:structure".into(), "Structure Reviewer: Reviews the structure axis.".into())], Some(".antiloki/context/laws.md"));
        assert!(md.starts_with(RULES_MARK), "{md}");
        assert!(md.contains("- **TDD** - write the test first then the code -> `.cursor/skills/tdd/SKILL.md`"), "{md}");
        assert!(md.contains("## Review specialists - apply each lens to your own diff before you report done"));
        assert!(md.contains("- `axis:structure` - Structure Reviewer: Reviews the structure axis."));
        assert!(md.contains("`.antiloki/context/laws.md`"));
        let lean = render_rules(".agents/skills", &[], &[], None);
        assert!(!lean.contains("## Skills") && !lean.contains("## Laws"), "a section with nothing in it is not written: {lean}");
    }

    // ── F-16 (audit 2026-10-01) ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("antiloki-seat-files-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }

    #[test]
    fn a_name_or_description_is_one_line_in_the_rules_file_too() {
        assert_eq!(one_line("a\nb\r\nc\u{2028}d\te"), "a b  c d e".replace("  ", "  "));
        assert_eq!(one_line("  padded \n"), "padded");
        let md = render_rules(".cursor/skills", &[skill("tdd", "TDD\n- **fake** - injected", "d\nescription: x")], &[("id\nx".into(), "what\n## Laws".into())], None);
        assert_eq!(md.lines().filter(|l| l.starts_with("- **")).count(), 1, "{md}");
        assert!(!md.lines().any(|l| l.starts_with("## Laws")), "{md}");
        assert_eq!(md.lines().filter(|l| l.starts_with("- `")).count(), 1, "{md}");
    }

    #[cfg(unix)]
    #[test]
    fn write_contained_refuses_a_symlinked_folder_or_file_that_leaves_the_root_and_writes_atomically() {
        let root = tmp("wc-root"); let out = tmp("wc-out");
        // a folder that is a link out
        std::os::unix::fs::symlink(&out, root.join("linked")).unwrap();
        let e = write_contained(&root, &root.join("linked/sub/f.txt"), b"x").unwrap_err();
        assert!(e.contains("outside the workspace"), "{e}");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "nothing was created outside, not even the missing sub folder");
        // a file that is a link out, and a dangling one
        std::fs::write(out.join("victim"), "precious").unwrap();
        std::os::unix::fs::symlink(out.join("victim"), root.join("f.txt")).unwrap();
        assert!(write_contained(&root, &root.join("f.txt"), b"x").unwrap_err().contains("symlink"));
        assert_eq!(std::fs::read_to_string(out.join("victim")).unwrap(), "precious");
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();
        assert!(write_contained(&root, &root.join("dangling"), b"x").is_err());
        // a link that stays inside is written through and stays a link; a plain path is created with its folders, via a temp file that is gone afterwards
        std::fs::write(root.join("real.md"), "old").unwrap();
        std::os::unix::fs::symlink(root.join("real.md"), root.join("alias.md")).unwrap();
        write_contained(&root, &root.join("alias.md"), b"new").unwrap();
        assert_eq!(std::fs::read_to_string(root.join("real.md")).unwrap(), "new");
        assert!(std::fs::symlink_metadata(root.join("alias.md")).unwrap().file_type().is_symlink());
        write_contained(&root, &root.join("a/b/c.txt"), b"deep").unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a/b/c.txt")).unwrap(), "deep");
        assert!(std::fs::read_dir(root.join("a/b")).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains("tmp")));
        let _ = std::fs::remove_dir_all(&root); let _ = std::fs::remove_dir_all(&out);
    }

    #[cfg(unix)]
    #[test]
    fn the_rules_file_is_not_written_through_a_symlinked_rules_folder() {
        let wt = tmp("rules-wt"); let out = tmp("rules-out");
        std::fs::create_dir_all(wt.join(".cursor")).unwrap();
        std::os::unix::fs::symlink(&out, wt.join(".cursor/rules")).unwrap();
        let db = Connection::open_in_memory().unwrap();
        let pane = json!({ "pane_kind": "cursor", "settings_json": "{}" });
        let r = write_rules(&db, &pane, &wt, &[skill("tdd", "TDD", "d")]);
        assert!(r["skipped"].as_str().map_or(false, |s| s.contains("outside the workspace")), "{r}");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "nothing was written into the folder the link points at");
        let _ = std::fs::remove_dir_all(&wt); let _ = std::fs::remove_dir_all(&out);
    }
}
