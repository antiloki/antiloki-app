//! THE ASYNC TWINS of `antiloki_rs::proc`.
//!
//! Everything about which program the OS will actually start, and how a path is quoted, lives in the core
//! crate — `src/` spawns 220 times of its own and needs the same answers. Only these two constructors live
//! here, because they are `tokio::process` and the core crate has no tokio and should not grow one for this.
pub use antiloki_rs::proc::{cmd, quote, quote_all, resolve, shell_parts, shell_sync, which, search_report};

/// The async twin of `proc::cmd`, for the request paths that await output.
pub fn tokio_cmd(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut c = tokio::process::Command::new(resolve(program.as_ref()));
    #[cfg(windows)]
    { c.creation_flags(antiloki_rs::proc::NO_WINDOW); }
    c
}

/// Run one command LINE (not a program plus arguments) — the project's own test or build command, as written
/// in its own manifest. `cmd /C` takes the whole line the same way `sh -c` does.
pub fn shell(line: &str) -> tokio::process::Command {
    let (sh, flag) = shell_parts();
    let mut c = tokio_cmd(sh);
    c.arg(flag).arg(line);
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_async_shell_is_the_platforms_own_too() {
        let (sh, _) = shell_parts();
        let got = shell("npm test").as_std().get_program().to_string_lossy().to_lowercase();
        assert!(got.contains(sh), "{got} is not {sh}");
    }
}
