//! byok-local-endpoint-set.rs — twin of skills/system/byok-local-endpoint-set.mjs (real
//! filesystem side effect, NOT db-touching — the `_db` parameter is accepted only to match the
//! standard skill signature and is otherwise unused, mirroring the JS `byokLocalEndpointSet(input)`
//! which never reads `context` at all). NAMES ARE CONTRACT: `byokLocalEndpointSet` (default; JS
//! `byokLocalEndpointSet`).
//!
//! Persists the local AI server location (the Ollama base URL) to the gitignored auth dir
//! (`HOME_DIR/auth/ollama.host`, mode 0o600) so the ollama-call adapter knows where to reach it.
//! Mirrors byok-key-set: the value never lands in a database or a response payload.
//!
//! Per CONVERSION-PLAYBOOK lesson 3 (and the already-converted sibling
//! skills/system/byok-key-set.rs): HOME_DIR/AUTH_DIR resolution is INLINED here rather than
//! importing roots.mjs (`ANTILOKI_HOME` env override, else Node `os.homedir()` semantics: POSIX
//! `$HOME`; Windows `%USERPROFILE%` falling back to `%HOMEDRIVE%%HOMEPATH%`). roots.mjs:
//! `AUTH_DIR = join(HOME_DIR, 'auth')`; this module's own `HOST_FILE = resolve(AUTH_DIR,
//! 'ollama.host')`.
//!
//! Because this does REAL filesystem I/O (mkdir + writeFile with a POSIX mode bit), it is
//! UNVERIFIABLE through the conform-db fixture harness (no real-fs-mutation assertion exists
//! there) — this unit needs BESPOKE NATIVE TESTING, not conform-db fixturing (see manifest, and
//! byok-key-set.rs precedent). Proven below via `#[cfg(test)]`, copying the `with_home()`
//! env-var + tempdir-injection pattern from byok-key-set.rs exactly.
//!
//! Windows note: `writeFileSync(path, data, { mode: 0o600 })` is a POSIX permission bit Node
//! applies via the underlying `open()` syscall — Windows has no such POSIX mode concept, so on
//! Windows Node's `mode` option is effectively inert for real permission enforcement. This twin
//! applies the mode bit via `std::os::unix::fs::OpenOptionsExt::mode` on unix targets only; on
//! Windows it writes the file without a mode argument — matching what Node actually achieves
//! per-platform, not a divergence introduced by this port.
//!
//! Throw fidelity: the two JS guard-throws (`err.type` from engine/error.mjs) are reproduced as
//! `Err(String)` carrying the EXACT `AntilokiError` message text (`"<skill>: <cause>"` — see
//! error.mjs `parts.join(' ')`), verified against real JS via
//! `node --test skills/system/byok-local-endpoint-set.test.mjs`. Note the FIRST guard's `got`
//! is the RAW `input?.endpoint` (pre-trim), matching JS `err.type(..., input?.endpoint)`; the
//! SECOND guard's `got` is always typeof `"string"` since by then `endpoint` is guaranteed a
//! string (only the typeof is rendered into the message, per error.mjs `err.type`).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki');
// AUTH_DIR = join(HOME_DIR, 'auth'); HOST_FILE = resolve(AUTH_DIR, 'ollama.host').
fn host_file() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("auth").join("ollama.host")
}

// = JS `typeof v` for JSON-representable values (feeds the endpoint type-guard messages).
fn type_of(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) | Some(Value::Array(_)) | Some(Value::Object(_)) => "object",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
    }
}

// JS: /^https?:\/\//i.test(endpoint)
fn has_http_scheme(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

pub fn byokLocalEndpointSet(input: &Value, _db: &Connection) -> Result<Value, String> {
    // JS: typeof input?.endpoint === 'string' ? input.endpoint.trim().replace(/\/+$/, '') : ''
    let raw = input.as_object().and_then(|o| o.get("endpoint"));
    let endpoint: String = match raw {
        Some(Value::String(s)) => s.trim().trim_end_matches('/').to_string(),
        _ => String::new(),
    };

    if endpoint.is_empty() {
        return Err(format!(
            "byok-local-endpoint-set: endpoint must be non-empty URL (e.g. http://localhost:11434) (got {})",
            type_of(raw)
        ));
    }
    if !has_http_scheme(&endpoint) {
        return Err(
            "byok-local-endpoint-set: endpoint must be an http(s):// URL (got string)".to_string(),
        );
    }

    let path = host_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    write_host_file(&path, &endpoint)?;

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("endpoint".into(), json!(endpoint));
    Ok(Value::Object(out))
}

#[cfg(unix)]
fn write_host_file(path: &std::path::Path, contents: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(contents.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn write_host_file(path: &std::path::Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default, and this same var is read by other converted twins' tests too.
    // shared crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race
    // with OTHER files' tests over the same real env var.

    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "antiloki-byok-local-endpoint-set-rs-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn writes_trimmed_endpoint_with_mode_0o600() {
        let dir = temp_dir("write");
        let r = with_home(dir.to_str().unwrap(), || {
            byokLocalEndpointSet(&json!({ "endpoint": "  http://localhost:11434  " }), &db0()).unwrap()
        });
        assert_eq!(r, json!({ "ok": true, "endpoint": "http://localhost:11434" }));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "endpoint"]);
        let host_path = dir.join("auth").join("ollama.host");
        assert_eq!(fs::read_to_string(&host_path).unwrap(), "http://localhost:11434");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&host_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn strips_trailing_slashes() {
        let dir = temp_dir("trailing-slash");
        let r = with_home(dir.to_str().unwrap(), || {
            byokLocalEndpointSet(&json!({ "endpoint": "https://example.com:8080///" }), &db0()).unwrap()
        });
        assert_eq!(r["endpoint"], json!("https://example.com:8080"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_uppercase_http_scheme() {
        let dir = temp_dir("uppercase-scheme");
        let r = with_home(dir.to_str().unwrap(), || {
            byokLocalEndpointSet(&json!({ "endpoint": "HTTPS://EXAMPLE.COM" }), &db0()).unwrap()
        });
        assert_eq!(r["endpoint"], json!("HTTPS://EXAMPLE.COM"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_missing_endpoint() {
        let dir = temp_dir("missing-endpoint");
        let e = with_home(dir.to_str().unwrap(), || {
            byokLocalEndpointSet(&json!({}), &db0()).unwrap_err()
        });
        assert_eq!(
            e,
            "byok-local-endpoint-set: endpoint must be non-empty URL (e.g. http://localhost:11434) (got undefined)"
        );
        assert!(!dir.join("auth").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_non_string_endpoint() {
        let dir = temp_dir("non-string-endpoint");
        let e = with_home(dir.to_str().unwrap(), || {
            byokLocalEndpointSet(&json!({ "endpoint": 12345 }), &db0()).unwrap_err()
        });
        assert_eq!(
            e,
            "byok-local-endpoint-set: endpoint must be non-empty URL (e.g. http://localhost:11434) (got number)"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_whitespace_only_endpoint() {
        let dir = temp_dir("whitespace-endpoint");
        let e = with_home(dir.to_str().unwrap(), || {
            byokLocalEndpointSet(&json!({ "endpoint": "   " }), &db0()).unwrap_err()
        });
        assert_eq!(
            e,
            "byok-local-endpoint-set: endpoint must be non-empty URL (e.g. http://localhost:11434) (got string)"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_non_http_scheme() {
        let dir = temp_dir("non-http-scheme");
        let e = with_home(dir.to_str().unwrap(), || {
            byokLocalEndpointSet(&json!({ "endpoint": "ftp://example.com" }), &db0()).unwrap_err()
        });
        assert_eq!(
            e,
            "byok-local-endpoint-set: endpoint must be an http(s):// URL (got string)"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn null_input_treated_as_empty_object() {
        let dir = temp_dir("null-input");
        let e = with_home(dir.to_str().unwrap(), || {
            byokLocalEndpointSet(&Value::Null, &db0()).unwrap_err()
        });
        assert_eq!(
            e,
            "byok-local-endpoint-set: endpoint must be non-empty URL (e.g. http://localhost:11434) (got undefined)"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
