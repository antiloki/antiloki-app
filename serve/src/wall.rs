//! THE WALL AROUND A GROUP MEMBER ON ANY PROVIDER (operator 2026-11-27: "the full groups is not finished. It should finish in all providers"). Claude Code is held by its pre-write hook; the other CLIs have no such
//! door, so the operating system holds them (Linux, `bwrap`): the whole machine is READ-ONLY to the call, the folders with keys and the other CLIs' sign-ins are empty, and the only places it can write are its own CLI's
//! state (never that CLI's settings) and, for a member that was given files, a COPY of the worktree laid over the worktree's own path. When it ends, only the changes inside its scope are brought back.
use std::path::{Path, PathBuf};

/// What one sandboxed call sees.
#[derive(Clone, Debug, Default)]
pub struct Wall { pub work: PathBuf, /** laid over `work`, writable; None: `work` stays read-only */ pub copy: Option<PathBuf>, pub state: Vec<PathBuf>, pub locked: Vec<PathBuf>, pub hidden: Vec<PathBuf>, /** single files the run writes under /tmp */ pub files: Vec<PathBuf> }

pub fn available() -> Result<(), String> { if !cfg!(target_os = "linux") { return Err("on this system only Claude Code members can be held to some files".into()); } crate::proc::which("bwrap").map(|_| ()).ok_or_else(|| "`bwrap` (bubblewrap) is not installed: it is what holds members on this provider".into()) }

/// (writable state, settings kept read-only inside it) of one CLI, relative to the home folder. Pure.
pub fn state_of(engine: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match engine {
        "codex" => (&[".codex", ".cache/codex"], &[".codex/config.toml", ".codex/hooks.json", ".codex/skills", ".codex/prompts", ".codex/AGENTS.md", ".codex/rules"]),
        "gemini" => (&[".gemini", ".cache/agy", ".cache/antigravity"], &[".gemini/settings.json", ".gemini/antigravity-cli/settings.json", ".gemini/antigravity-cli/mcp_config.json", ".gemini/config", ".gemini/extensions", ".gemini/GEMINI.md"]),
        "cursor" => (&[".cursor", ".config/cursor", ".local/share/cursor-agent", ".cache/cursor-agent"], &[".cursor/mcp.json", ".cursor/hooks.json", ".cursor/rules", ".cursor/cli-config.json", ".local/share/cursor-agent/versions"]),
        "opencode" | "opencode-direct" => (&[".local/share/opencode", ".local/state/opencode", ".cache/opencode"], &[".local/share/opencode/bin"]),
        "grok" => (&[".grok", ".cache/grok"], &[".grok/config.toml", ".grok/user-settings.json"]),
        _ => (&[], &[]),
    }
}
/// Folders no member sees into: keys, and the sign-ins of the CLIs it does not run on. Pure.
pub fn hidden_for(engine: &str) -> Vec<&'static str> {
    let mine = state_of(engine).0;
    let mut v = vec![".ssh", ".gnupg", ".aws", ".azure", ".kube", ".config/gcloud", ".config/gh", ".password-store", ".local/share/keyrings", ".mozilla", ".config/google-chrome", ".config/chromium", ".config/BraveSoftware", ".antiloki/tokens", ".claude"];
    for e in ["codex", "gemini", "cursor", "opencode", "grok"] { for p in state_of(e).0 { if !mine.contains(p) { v.push(p); } } }
    v
}
/// The wall for one call of `engine` in `work`, given the home folder. Only paths that exist are named.
pub fn build(engine: &str, home: &Path, work: &Path, copy: Option<PathBuf>) -> Wall {
    let have = |v: &[&str]| v.iter().map(|p| home.join(p)).filter(|p| p.exists()).collect::<Vec<_>>();
    let (state, locked) = state_of(engine);
    Wall { work: work.to_path_buf(), copy, state: have(state), locked: have(locked), hidden: have(&hidden_for(engine)), files: vec![] }
}
/// The arguments of `bwrap` that run `bin args` behind this wall. Order matters: what comes later covers what came before. Pure.
pub fn args(w: &Wall, bin: &str, args: &[String]) -> Vec<String> {
    let mut a: Vec<String> = ["--die-with-parent", "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"].map(String::from).to_vec();
    let s = |p: &Path| p.to_string_lossy().to_string();
    for p in &w.hidden { if p.is_dir() { a.extend(["--tmpfs".to_string(), s(p)]); } }
    for p in &w.state { a.extend(["--bind".to_string(), s(p), s(p)]); }
    for p in &w.locked { a.extend(["--ro-bind".to_string(), s(p), s(p)]); }
    for f in &w.files { a.extend(["--bind".to_string(), s(f), s(f)]); }
    // the worktree last: it wins over anything above, wherever it lives (some worktrees live under a hidden folder)
    match &w.copy { Some(c) => a.extend(["--bind".to_string(), s(c), s(&w.work)]), None => a.extend(["--ro-bind".to_string(), s(&w.work), s(&w.work)]) }
    a.extend(["--chdir".to_string(), s(&w.work), "--".to_string(), bin.to_string()]); a.extend(args.iter().cloned()); a
}

/// A private copy of the worktree for a member to work in (instant on a filesystem that shares blocks). Blocking.
pub fn copy_of(work: &Path) -> Result<PathBuf, String> {
    let base = std::env::var_os("ANTILOKI_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".antiloki"))).unwrap_or_else(std::env::temp_dir).join("members");
    let to = base.join(format!("{}-{}", std::process::id(), crate::cli::now_ms())); std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
    let o = std::process::Command::new("cp").args(["-a", "--reflink=auto", "--"]).arg(work.join(".")).arg(&to).output().map_err(|e| format!("cp: {e}"))?;
    if !o.status.success() { let _ = std::fs::remove_dir_all(&to); return Err(format!("the worktree could not be copied: {}", String::from_utf8_lossy(&o.stderr).chars().take(300).collect::<String>())); }
    Ok(to)
}
const NEVER: [&str; 10] = [".git", ".antiloki", ".claude", ".codex", ".cursor", ".opencode", ".grok", ".mcp.json", "agents.md", "claude.md"];
/// Every file and link under `root` as relative paths; `.git` is not looked into.
fn walk(root: &Path, at: &Path, out: &mut std::collections::BTreeSet<String>) {
    for e in std::fs::read_dir(at).into_iter().flatten().flatten() {
        let p = e.path(); let Ok(rel) = p.strip_prefix(root) else { continue }; if rel.as_os_str() == ".git" { continue }
        match e.file_type() { Ok(t) if t.is_dir() => walk(root, &p, out), Ok(_) => { out.insert(rel.to_string_lossy().to_string()); } Err(_) => {} }
    }
}
fn same(a: &Path, b: &Path) -> bool {
    let (Ok(x), Ok(y)) = (a.symlink_metadata(), b.symlink_metadata()) else { return false };
    if x.file_type().is_symlink() || y.file_type().is_symlink() { return std::fs::read_link(a).ok() == std::fs::read_link(b).ok(); }
    x.len() == y.len() && (x.modified().ok() == y.modified().ok() || std::fs::read(a).ok() == std::fs::read(b).ok())
}
/// Is `rel` under `work` really inside it (no link on the way leads out)?
fn stays_inside(work: &Path, rel: &str) -> bool { let Ok(root) = std::fs::canonicalize(work) else { return false }; let mut at = work.join(rel); loop { if let Ok(real) = std::fs::canonicalize(&at) { return real.starts_with(&root); } if at.symlink_metadata().is_ok() { return false; } if !at.pop() { return false; } } }
/// BRING BACK ONLY WHAT THE MEMBER WAS GIVEN: files changed, made or removed in the copy are applied to the worktree when `allowed` says so; links and the folders agents and git run from never are. Returns
/// (brought back, refused). Blocking.
pub fn bring_back(work: &Path, copy: &Path, allowed: impl Fn(&str) -> bool) -> (Vec<String>, Vec<String>) {
    let (mut there, mut mine) = Default::default(); walk(work, work, &mut there); walk(copy, copy, &mut mine);
    let (there, mine): (std::collections::BTreeSet<String>, std::collections::BTreeSet<String>) = (there, mine);
    let (mut done, mut refused) = (vec![], vec![]);
    for rel in there.union(&mine) {
        let (w, c) = (work.join(rel), copy.join(rel));
        if mine.contains(rel) && there.contains(rel) && same(&w, &c) { continue; }
        let head = rel.split('/').next().unwrap_or("").to_ascii_lowercase();
        let link = c.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink());
        if NEVER.contains(&head.as_str()) { if mine.contains(rel) && head != ".antiloki" { refused.push(rel.clone()); } continue; }
        if link || !allowed(rel) || !stays_inside(work, rel) { refused.push(rel.clone()); continue; }
        let ok = if mine.contains(rel) { w.parent().map(std::fs::create_dir_all).transpose().is_ok() && std::fs::copy(&c, &w).is_ok() } else { std::fs::remove_file(&w).is_ok() };
        if ok { done.push(rel.clone()) } else { refused.push(rel.clone()) }
    }
    (done, refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("antiloki-wall-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test]
    fn only_the_changes_inside_the_scope_come_back() {
        let work = tmp("work"); let w = |p: &str, t: &str| { let f = work.join(p); std::fs::create_dir_all(f.parent().unwrap()).unwrap(); std::fs::write(f, t).unwrap(); };
        w("docs/a.md", "one"); w("docs/gone.md", "x"); w("src/main.rs", "fn main(){}"); w("AGENTS.md", "rules");
        let copy = copy_of(&work).unwrap(); let c = |p: &str, t: &str| { let f = copy.join(p); std::fs::create_dir_all(f.parent().unwrap()).unwrap(); std::fs::write(f, t).unwrap(); };
        c("docs/a.md", "two!"); c("docs/new/b.md", "b"); std::fs::remove_file(copy.join("docs/gone.md")).unwrap(); c("src/main.rs", "evil"); c("AGENTS.md", "mine"); c(".claude/settings.json", "{}");
        #[cfg(unix)] std::os::unix::fs::symlink("/etc/passwd", copy.join("docs/link")).unwrap();
        let (done, refused) = bring_back(&work, &copy, |p| p.starts_with("docs/") || p == "AGENTS.md");
        assert_eq!(done, vec!["docs/a.md".to_string(), "docs/gone.md".into(), "docs/new/b.md".into()]);
        for r in ["src/main.rs", "AGENTS.md", ".claude/settings.json"] { assert!(refused.contains(&r.to_string()), "{r}: {refused:?}"); }
        #[cfg(unix)] assert!(refused.contains(&"docs/link".to_string()) && !work.join("docs/link").exists());
        assert_eq!(std::fs::read_to_string(work.join("src/main.rs")).unwrap(), "fn main(){}"); assert_eq!(std::fs::read_to_string(work.join("docs/a.md")).unwrap(), "two!"); assert!(!work.join("docs/gone.md").exists());
        let _ = std::fs::remove_dir_all(&work); let _ = std::fs::remove_dir_all(&copy);
    }
    #[test]
    fn the_wall_locks_settings_hides_keys_and_other_sign_ins_and_lays_the_copy_last() {
        assert!(hidden_for("codex").contains(&".ssh") && hidden_for("codex").contains(&".cursor") && !hidden_for("codex").contains(&".codex") && hidden_for("cursor").contains(&".codex"));
        let w = Wall { work: "/w".into(), copy: Some("/c".into()), state: vec!["/h/.codex".into()], locked: vec!["/h/.codex/config.toml".into()], hidden: vec![], files: vec!["/tmp/last".into()] };
        let a = args(&w, "codex", &["exec".to_string()]); let at = |x: &str| a.iter().position(|y| y == x).unwrap();
        assert!(at("/h/.codex/config.toml") > at("/h/.codex") && at("/c") > at("/h/.codex/config.toml"), "{a:?}"); assert_eq!(&a[a.len() - 3..], ["--", "codex", "exec"]);
        let r = args(&Wall { work: "/w".into(), ..Default::default() }, "x", &[]); assert!(r.windows(3).any(|v| v == ["--ro-bind", "/w", "/w"]));
    }
    /// The real wall, on a machine that has it: the call cannot write outside, cannot read a hidden folder, and writes into the copy.
    #[test]
    fn a_real_process_behind_the_wall_writes_only_into_its_copy() {
        if available().is_err() { return; }
        let home = tmp("home"); std::fs::create_dir_all(home.join(".ssh")).unwrap(); std::fs::write(home.join(".ssh/id"), "secret").unwrap(); std::fs::write(home.join("note"), "mine").unwrap();
        // the worktree stands outside /tmp, which the wall replaces
        let base = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap().join("target")).join(format!("wall-it-{}", std::process::id())); let work = base.join("work"); let copy = base.join("copy"); std::fs::create_dir_all(&work).unwrap(); std::fs::create_dir_all(&copy).unwrap();
        let h = base.join("home"); std::fs::create_dir_all(h.join(".ssh")).unwrap(); std::fs::write(h.join(".ssh/id"), "secret").unwrap(); std::fs::write(h.join("note"), "mine").unwrap();
        let wall = build("codex", &h, &work, Some(copy.clone()));
        let script = format!("echo in > made.txt; echo no > {}/note 2>/dev/null && echo WROTE-OUT; cat {}/.ssh/id 2>/dev/null; echo end", h.display(), h.display());
        let o = std::process::Command::new("bwrap").args(args(&wall, "sh", &["-c".to_string(), script])).output().unwrap(); let out = String::from_utf8_lossy(&o.stdout);
        assert!(out.contains("end") && !out.contains("WROTE-OUT") && !out.contains("secret"), "{out} {}", String::from_utf8_lossy(&o.stderr));
        assert!(copy.join("made.txt").is_file() && !work.join("made.txt").exists()); assert_eq!(std::fs::read_to_string(h.join("note")).unwrap(), "mine");
        let _ = std::fs::remove_dir_all(&base); let _ = std::fs::remove_dir_all(&home);
    }
}
