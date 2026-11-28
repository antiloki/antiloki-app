//! byok-key-set.rs — twin of skills/system/byok-key-set.mjs (real filesystem side effect, NOT
//! db-touching — the `_db` parameter is accepted only to match the standard skill signature and
//! is otherwise unused, mirroring the JS `byokKeySet(input, context)` which never reads
//! `context`). NAMES ARE CONTRACT: `byokKeySet` (default; JS `byokKeySet`).
//!
//! Persists a bring-your-own-key API key to the gitignored auth dir
//! (`HOME_DIR/auth/<provider>.key`, mode 0o600) so a provider adapter (e.g. openrouter-call) can
//! read it as a fallback. The key never lands in a database or a response payload.
//!
//! Per CONVERSION-PLAYBOOK lesson 3: HOME_DIR/AUTH_DIR resolution is INLINED here rather than
//! importing roots.mjs — mirrors the already-converted skills/agents/god/god-driver-resume.rs
//! (`ANTILOKI_HOME` env override, else Node `os.homedir()` semantics: POSIX `$HOME`; Windows
//! `%USERPROFILE%` falling back to `%HOMEDRIVE%%HOMEPATH%`). roots.mjs:
//! `AUTH_DIR = join(HOME_DIR, 'auth')`.
//!
//! Because this does REAL filesystem I/O (mkdir + writeFile with a POSIX mode bit), it is
//! UNVERIFIABLE through the conform-db fixture harness (no real-fs-mutation assertion exists
//! there) — this unit needs BESPOKE NATIVE TESTING, not conform-db fixturing (see manifest).
//! Proven below via `#[cfg(test)]`, copying the `with_home()` env-var + tempdir-injection pattern
//! from god-driver-resume.rs exactly.
//!
//! Windows note: `writeFileSync(path, data, { mode: 0o600 })` is a POSIX permission bit Node
//! applies via the underlying `open()` syscall — Windows has no such POSIX mode concept, so on
//! Windows Node's `mode` option is effectively inert for real permission enforcement (the
//! filesystem ACL model doesn't map to unix rwx bits). This twin applies the mode bit via
//! `std::os::unix::fs::OpenOptionsExt::mode` on unix targets only; on Windows it writes the file
//! without a mode argument (no unix-mode API exists there either) — matching what Node actually
//! achieves per-platform, not a divergence introduced by this port.
//!
//! Throw fidelity: the two JS guard-throws (`err.enumVal` / `err.type` from engine/error.mjs) are
//! reproduced as `Err(String)` carrying the EXACT `AntilokiError` message text
//! (`"<skill>: <cause>"` — see error.mjs `parts.join(' ')`), verified against real JS via
//! `node --test skills/system/byok-key-set.test.mjs`.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

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
// AUTH_DIR = join(HOME_DIR, 'auth').
fn auth_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("auth")
}

// KEY_FILES: provider → key filename. Adapters read the matching file as a fallback.
fn key_filename(provider: &str) -> Option<&'static str> {
    match provider {
        "openrouter" => Some("openrouter.key"),
        _ => None,
    }
}

// = JS `${v}` (template-literal interpolation; feeds the enumVal guard message).
fn tmpl(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        n @ Value::Number(_) => n.to_string(),
        Value::Array(a) => a.iter().map(tmpl).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

// = JS `typeof v` for JSON-representable values (feeds the api_key type-guard message).
fn type_of(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) | Some(Value::Array(_)) | Some(Value::Object(_)) => "object",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
    }
}

pub fn byokKeySet(input: &Value, _db: &Connection) -> Result<Value, String> {
    // JS: const { provider = 'openrouter', api_key } = input ?? {};  the default fires on
    // `undefined` ONLY — an explicit `null`/other provider value is passed through untouched.
    let obj = input.as_object();
    let provider_v = obj.and_then(|o| o.get("provider"));
    let provider: Value = match provider_v {
        None => json!("openrouter"),
        Some(v) => v.clone(),
    };
    let api_key_v = obj.and_then(|o| o.get("api_key"));

    // JS: KEY_FILES[provider] — object property lookup coerces `provider` to a string key.
    // DISCLOSED GAP: KEY_FILES has exactly one key ("openrouter"), so in practice only a
    // `provider` that IS the string "openrouter" matches; every other JSON type falls into the
    // same "not found" branch below, which is what this twin does directly. The one JS edge case
    // not reproduced is an array whose `Array.prototype.toString()` happens to render
    // "openrouter" (e.g. `["openrouter"]`) — JS's implicit ToPropertyKey coercion WOULD match
    // that; this twin (comparing `provider.as_str()` only) will not. Not exercised by any
    // realistic caller.
    let filename = match provider.as_str().and_then(key_filename) {
        Some(f) => f,
        None => {
            return Err(format!(
                "byok-key-set: provider must be one of: openrouter (got '{}')",
                tmpl(&provider)
            ));
        }
    };

    // JS: typeof api_key !== 'string' || !api_key.trim()
    let api_key = match api_key_v {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        _ => {
            return Err(format!(
                "byok-key-set: api_key must be non-empty string (got {})",
                type_of(api_key_v)
            ));
        }
    };

    let dir = auth_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let key_path = dir.join(filename);
    write_key_file(&key_path, api_key.trim())?;

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("provider".into(), provider);
    out.insert("key_present".into(), json!(true));
    Ok(Value::Object(out))
}

#[cfg(unix)]
fn write_key_file(path: &Path, contents: &str) -> Result<(), String> {
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
fn write_key_file(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default, and this same var is read by other converted twins' tests too.
    // shared crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race with OTHER files' tests over the same real env var.

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
        dir.push(format!("antiloki-byok-key-set-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn writes_trimmed_key_with_mode_0o600() {
        let dir = temp_dir("write");
        let r = with_home(dir.to_str().unwrap(), || {
            byokKeySet(&json!({ "provider": "openrouter", "api_key": "  sk-abc123  " }), &db0()).unwrap()
        });
        assert_eq!(r, json!({ "ok": true, "provider": "openrouter", "key_present": true }));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "provider", "key_present"]);
        let key_path = dir.join("auth").join("openrouter.key");
        assert_eq!(fs::read_to_string(&key_path).unwrap(), "sk-abc123");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults_provider_to_openrouter_when_omitted() {
        let dir = temp_dir("default-provider");
        let r = with_home(dir.to_str().unwrap(), || {
            byokKeySet(&json!({ "api_key": "sk-default" }), &db0()).unwrap()
        });
        assert_eq!(r["provider"], json!("openrouter"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_unknown_provider() {
        let dir = temp_dir("bad-provider");
        let e = with_home(dir.to_str().unwrap(), || {
            byokKeySet(&json!({ "provider": "bogus", "api_key": "x" }), &db0()).unwrap_err()
        });
        assert_eq!(e, "byok-key-set: provider must be one of: openrouter (got 'bogus')");
        assert!(!dir.join("auth").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_missing_api_key() {
        let dir = temp_dir("missing-key");
        let e = with_home(dir.to_str().unwrap(), || {
            byokKeySet(&json!({ "provider": "openrouter" }), &db0()).unwrap_err()
        });
        assert_eq!(e, "byok-key-set: api_key must be non-empty string (got undefined)");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_whitespace_only_api_key() {
        let dir = temp_dir("whitespace-key");
        let e = with_home(dir.to_str().unwrap(), || {
            byokKeySet(&json!({ "provider": "openrouter", "api_key": "   " }), &db0()).unwrap_err()
        });
        assert_eq!(e, "byok-key-set: api_key must be non-empty string (got string)");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn null_input_treated_as_empty_object() {
        let dir = temp_dir("null-input");
        let e = with_home(dir.to_str().unwrap(), || byokKeySet(&Value::Null, &db0()).unwrap_err());
        assert_eq!(e, "byok-key-set: api_key must be non-empty string (got undefined)");
        let _ = fs::remove_dir_all(&dir);
    }
}
