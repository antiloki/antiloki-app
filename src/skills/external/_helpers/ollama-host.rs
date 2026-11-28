//! ollama-host.rs — twin of skills/external/_helpers/ollama-host.mjs (leaf helper, no db).
//! Resolves the local Ollama server endpoint so metadata readers (byok-local-models-list) can
//! share it without importing the ollama-call.mjs provider adapter (AI_CALL_THROUGH_ENVELOPE
//! forbids adapter imports outside ai/ and external/). NAMES ARE CONTRACT: `resolveHost`.
//!
//! Precedence: `OLLAMA_HOST` env (trimmed, trailing slashes stripped) > the
//! `.antiloki/auth/ollama.host` file contents (same normalization) > `http://localhost:11434`.
//!
//! roots.mjs's AUTH_DIR = HOME_DIR/auth, where HOME_DIR = resolve($ANTILOKI_HOME) if set, else
//! join(os.homedir(), '.antiloki'). This crate has no `dirs`-style dependency, so homedir is
//! resolved directly from the same platform env vars Node's os.homedir() itself reads under the
//! hood (POSIX: $HOME; Windows: %USERPROFILE%, falling back to %HOMEDRIVE%%HOMEPATH%) — real
//! behavior without pulling in a new crate for one leaf helper.

use std::env;
use std::fs;
use std::path::PathBuf;

const DEFAULT_HOST: &str = "http://localhost:11434";

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki');
// AUTH_DIR = join(HOME_DIR, 'auth'). resolve()/join() both collapse to a plain path join here —
// callers of this helper never pass a relative or '..'-laden ANTILOKI_HOME in practice.
fn auth_dir() -> PathBuf {
    let home_dir = match env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("auth")
}

fn host_file() -> PathBuf {
    auth_dir().join("ollama.host")
}

// JS: `str.replace(/\/+$/, '')` — strip one-or-more trailing slashes.
fn strip_trailing_slashes(s: &str) -> &str {
    s.trim_end_matches('/')
}

/// JS: `resolveHost()`.
pub fn resolveHost() -> String {
    if let Ok(env_host) = env::var("OLLAMA_HOST") {
        let trimmed = env_host.trim();
        if !trimmed.is_empty() {
            return strip_trailing_slashes(trimmed).to_string();
        }
    }
    // JS: `existsSync(HOST_FILE)` then `readFileSync(...)` — collapsed here into one fallible
    // read; a file that exists but is unreadable (e.g. a directory at that path) falls through
    // to the default in this twin instead of surfacing an fs error, same end state the JS reaches
    // for "file doesn't exist" (existsSync false skips the read entirely either way).
    if let Ok(contents) = fs::read_to_string(host_file()) {
        let trimmed = contents.trim();
        if !trimmed.is_empty() {
            return strip_trailing_slashes(trimmed).to_string();
        }
    }
    DEFAULT_HOST.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // SAFETY: test-only env mutation (OLLAMA_HOST / ANTILOKI_HOME). Serialized via ENV_LOCK since
    // std::env is process-global and `cargo test` runs test fns on multiple threads by default.
    // shared crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race with OTHER files' tests over the same real env var.

    fn with_env<T>(ollama_host: Option<&str>, antiloki_home: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            match ollama_host {
                Some(v) => env::set_var("OLLAMA_HOST", v),
                None => env::remove_var("OLLAMA_HOST"),
            }
            match antiloki_home {
                Some(v) => env::set_var("ANTILOKI_HOME", v),
                None => env::remove_var("ANTILOKI_HOME"),
            }
        }
        let result = f();
        unsafe {
            env::remove_var("OLLAMA_HOST");
            env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = env::temp_dir();
        dir.push(format!("antiloki-ollama-host-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn env_host_wins_trimmed_and_slash_stripped() {
        with_env(Some("  http://myhost:1234//  "), None, || {
            assert_eq!(resolveHost(), "http://myhost:1234");
        });
    }

    #[test]
    fn blank_env_host_falls_through_to_default_with_no_host_file() {
        let dir = temp_dir("blank");
        with_env(Some("   "), Some(dir.to_str().unwrap()), || {
            assert_eq!(resolveHost(), DEFAULT_HOST);
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_env_falls_back_to_host_file_contents() {
        let dir = temp_dir("file");
        let auth = dir.join("auth");
        fs::create_dir_all(&auth).unwrap();
        fs::write(auth.join("ollama.host"), "  http://filehost:9999/  \n").unwrap();
        with_env(None, Some(dir.to_str().unwrap()), || {
            assert_eq!(resolveHost(), "http://filehost:9999");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_env_no_host_file_returns_default() {
        let dir = temp_dir("nofile");
        with_env(None, Some(dir.to_str().unwrap()), || {
            assert_eq!(resolveHost(), DEFAULT_HOST);
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_host_file_falls_back_to_default() {
        let dir = temp_dir("emptyfile");
        let auth = dir.join("auth");
        fs::create_dir_all(&auth).unwrap();
        fs::write(auth.join("ollama.host"), "   \n").unwrap();
        with_env(None, Some(dir.to_str().unwrap()), || {
            assert_eq!(resolveHost(), DEFAULT_HOST);
        });
        let _ = fs::remove_dir_all(&dir);
    }
}
