//! EVERY AGENT USES THE BUILD THAT IS INSTALLED (operator 2026-11-28: "all agents should use only new builds"). Hooks and the status line are started fresh each time; the tool helper lives as long as its agent, so after an
//! install it kept running the old program until the agent was restarted. It now becomes the new one itself, between two requests: same process and same pipes, so the agent notices nothing but the new tool list.
//! Windows has no way to replace a running process in place: there a helper changes when its agent restarts.
use std::path::PathBuf;

const CARRY: &str = "ANTILOKI_MCP_CARRY";

/// The program this helper was started as, and which file that was then. An install changes the file the path leads to.
pub struct Started { path: PathBuf, file: Option<(u64, u64, i64, u64)> }

fn file_of(p: &std::path::Path) -> Option<(u64, u64, i64, u64)> {
    #[cfg(unix)] { use std::os::unix::fs::MetadataExt; std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino(), m.mtime(), m.len())) }
    #[cfg(not(unix))] { let _ = p; None }
}

impl Started {
    pub fn now() -> Self { let path = std::env::args_os().next().map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_default(); let file = file_of(&path); Self { path, file } }

    /// If another build is installed at that path, become it; `request` (read, not yet answered) goes with it. Returns only when nothing changed, or the new file cannot run.
    pub fn hand_over(&self, request: &str) {
        let Some(was) = self.file else { return };
        if file_of(&self.path).map_or(true, |now| now == was) { return; }
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
            // a file still being written does not answer this: the old helper goes on
            if !std::process::Command::new(&self.path).arg("--version").stdin(std::process::Stdio::null()).output().is_ok_and(|o| o.status.success()) { return; }
            let carry = std::env::temp_dir().join(format!("antiloki-mcp-carry-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())));
            let wrote = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&carry).and_then(|mut f| f.write_all(request.as_bytes()));
            if wrote.is_ok() {
                let why = std::process::Command::new(&self.path).args(std::env::args_os().skip(1)).env(CARRY, &carry).exec();
                eprintln!("antiloki-agent: a newer build is installed but did not start: {why}");
            }
            let _ = std::fs::remove_file(&carry);
        }
        #[cfg(not(unix))] { let _ = request; }
    }
}

/// The request the build before this one read and did not answer.
pub fn carried() -> Option<String> {
    let path = std::env::var_os(CARRY)?; std::env::remove_var(CARRY);
    let line = std::fs::read_to_string(&path).ok(); let _ = std::fs::remove_file(&path); line
}
