//! auth-init.rs — twin of skills/external/auth-init.mjs. NAMES ARE CONTRACT: `authInit`
//! (default), `keyPath` (named export). `AUTH_DIR` is a plain roots.mjs re-exported const (not a
//! function — not a census unit per the crossing playbook), modeled as the `auth_dir()` fn, same
//! convention as sibling god-driver-halt.rs / byok-key-set.rs / tui-god-token.rs.
//!
//! Generates per-agent HMAC keys under `HOME_DIR/auth/<agent>.key` (roots.mjs `AUTH_DIR` =
//! `HOME_DIR/auth`, `HOME_DIR` = `$ANTILOKI_HOME` else `homedir()/.antiloki`) and auto-appends a
//! `.antiloki/auth/` ignore entry to `SUBSTRATE_ROOT/.gitignore` (roots.mjs `SUBSTRATE_ROOT` =
//! `$ANTILOKI_SUBSTRATE_ROOT` else self-derived from roots.mjs's own file location — inlined here
//! via `CARGO_MANIFEST_DIR`'s parent, same convention as source-list.rs / sandbox-resolve-path.rs,
//! since antiloki-core-rust and antiloki-core are siblings under the same SUBSTRATE_ROOT on this
//! checkout layout).
//!
//! BOTH resolved roots are real-fs-anchored FIXED paths (never fixture-controllable per the
//! crossing playbook's stack-facts.rs precedent), and the unit does real mkdir/write/chmod side
//! effects plus a genuinely-random 32-byte key — none of which is verifiable through the
//! conform-db fixture harness. BESPOKE NATIVE TESTING only (see manifest: no fixture, no
//! frontier entry). Proven below via `#[cfg(test)]`, copying the `with_home()` env-var +
//! tempdir-injection pattern from god-driver-halt.rs / byok-key-set.rs / tui-god-token.rs exactly,
//! extended with a paired `ANTILOKI_SUBSTRATE_ROOT` override for the `.gitignore` root.
//!
//! Windows note (same as byok-key-set.rs): `writeFileSync(path, data, { mode: 0o600 })` and
//! `chmodSync(path, 0o400)` are POSIX permission bits; Node's `mode` option / `chmodSync` are
//! inert for real enforcement on Windows (no unix rwx ACL mapping — the JS source's own comment:
//! "windows ignores most modes"). This twin applies `std::os::unix::fs` mode bits on unix targets
//! only; on Windows it writes/chmods without a mode argument, matching what Node actually
//! achieves per-platform, not a divergence introduced by this port.
//!
//! Randomness: `crypto.randomBytes(32)` is a real OS CSPRNG read. Neither `rand` nor `getrandom`
//! is (yet) a dependency of this crate, so this twin reaches the OS RNG directly with ZERO new
//! Cargo dependencies: `/dev/urandom` on unix; `RtlGenRandom` (advapi32.dll `SystemFunction036`
//! — the same legacy-named-but-still-fully-supported CSPRNG entry point the `getrandom` crate
//! itself historically used on Windows) via a minimal `extern "system"` FFI declaration, `#[link]`
//! resolved against the always-present Windows system library, no crate required.
//!
//! Throw fidelity: the one JS guard-throw — `err.range('auth-init', 'agent', '[a-z][a-z0-9_-]*',
//! a)` — only passes 4 of the 5 `err.range(skill, field, min, max, got)` params, so `max` receives
//! the invalid agent value and `got` is `undefined`; the real (if unintuitive) message is
//! `"auth-init: agent must be between [a-z][a-z0-9_-]* and <agent> (got undefined)"`. Verified
//! against real JS via `node --test skills/external/auth-init.test.mjs`. Reproduced verbatim as
//! `Err(String)`.
//!
//! Disclosed gap: `input.agent` (singular) is modeled as a JSON string (the realistic contract
//! per the module's own docstring — a CLI agent identifier like `dev`/`god`). A non-string truthy
//! `input.agent` value is JS-coercible (bare ToString, same as each `agents[]` element below) but
//! is not reproduced here — same class of narrow gap disclosed by byok-key-set.rs for its
//! `provider` lookup. `agents[]` array elements ARE coerced via `tmpl` (bare JS ToString, matching
//! the regex `.test()`/template-interpolation call sites) — including `null` → the string
//! `"null"`, not an empty string.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }

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

// roots.mjs: SUBSTRATE_ROOT = resolve($ANTILOKI_SUBSTRATE_ROOT) if set+truthy, else
// resolve(ENGINE_ROOT, '..') anchored at roots.mjs's own file location (antiloki-core's parent).
// CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent is that same SUBSTRATE_ROOT on this
// checkout layout (source-list.rs / sandbox-resolve-path.rs precedent).
fn substrate_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

// auth-init.mjs: const GITIGNORE_PATH = resolve(SUBSTRATE_ROOT, '.gitignore');
fn gitignore_path() -> PathBuf {
    substrate_root().join(".gitignore")
}

fn key_path_str(agent: &str) -> PathBuf {
    auth_dir().join(format!("{agent}.key"))
}

/// `export function keyPath(agent) { return resolve(AUTH_DIR, `${agent}.key`); }`
pub fn keyPath(agent: &Value) -> Value {
    json!(key_path_str(&tmpl(Some(agent))).to_string_lossy().into_owned())
}

/// `function ensureGitignore()` — appends the `.antiloki/auth/` ignore marker to
/// SUBSTRATE_ROOT/.gitignore unless a trimmed line already equals it exactly; returns whether it
/// wrote anything.
fn ensure_gitignore() -> Result<bool, String> {
    let marker = ".antiloki/auth/";
    let path = gitignore_path();
    let mut existing = String::new();
    if path.exists() {
        existing = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if existing.split('\n').any(|l| l.trim() == marker) {
            return Ok(false);
        }
    }
    let sep = if !existing.is_empty() && !existing.ends_with('\n') { "\n" } else { "" };
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    write!(f, "{sep}# Auth keys for agent identity (never commit)\n{marker}\n").map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(unix)]
fn os_random_bytes(n: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

#[cfg(windows)]
#[link(name = "advapi32")]
extern "system" {
    #[link_name = "SystemFunction036"]
    fn RtlGenRandom(buf: *mut u8, len: u32) -> u8;
}

#[cfg(windows)]
fn os_random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    let ok = unsafe { RtlGenRandom(buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return Err("RtlGenRandom failed".to_string());
    }
    Ok(buf)
}

#[cfg(unix)]
fn write_key_file(path: &Path, hex: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(hex.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn write_key_file(path: &Path, hex: &str) -> Result<(), String> {
    std::fs::write(path, hex.as_bytes()).map_err(|e| e.to_string())
}

// JS: `if (a === 'god') { try { chmodSync(p, 0o400) } catch { /* windows ignores most modes */ } }`
#[cfg(unix)]
fn restrict_to_read_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400));
}

#[cfg(not(unix))]
fn restrict_to_read_only(_path: &Path) {}

/// `export default function authInit(input, _context)` — `_context` is unused (JS destructures
/// only `input`), mirrored by the unused `_db` param below.
pub fn authInit(input: &Value, _db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    // JS: `if (agent) targets = [agent]` — truthy check (empty string/0/false/null all fall
    // through to the `agents` branch). See module doc: modeled as a JSON string.
    let agent = obj
        .and_then(|o| o.get("agent"))
        .filter(|v| truthy(Some(v)))
        .and_then(|v| v.as_str());
    let agents_arr = obj.and_then(|o| o.get("agents")).and_then(Value::as_array);
    let force = obj.and_then(|o| o.get("force")).map(|v| truthy(Some(v))).unwrap_or(false);

    let targets: Vec<String> = if let Some(a) = agent {
        vec![a.to_string()]
    } else if let Some(arr) = agents_arr.filter(|a| !a.is_empty()) {
        // = JS bare ToString (regex .test(a) / template `${a}.key`), NOT String(v ?? '') — a
        // null element stringifies to "null", not "". str_qq would collapse it to "" and fail
        // the [a-z][a-z0-9_-]* guard where real JS succeeds (targets:['dev', null] is valid).
        arr.iter().map(|v| tmpl(Some(v))).collect()
    } else {
        vec!["system".into(), "dev".into(), "qa".into(), "man".into(), "god".into()]
    };

    std::fs::create_dir_all(auth_dir()).map_err(|e| e.to_string())?;
    let gitignore_added = ensure_gitignore()?;

    let agent_re = regex::Regex::new(r"^[a-z][a-z0-9_-]*$").unwrap();
    let mut results: Vec<Value> = Vec::new();
    for a in &targets {
        if !agent_re.is_match(a) {
            // JS: err.range('auth-init', 'agent', '[a-z][a-z0-9_-]*', a) — only 4 of the 5
            // (skill, field, min, max, got) params are passed, so `max` = a and `got` is
            // undefined. See module doc for the verified real message.
            return Err(format!(
                "auth-init: agent must be between [a-z][a-z0-9_-]* and {a} (got undefined)"
            ));
        }
        let p = key_path_str(a);
        let exists = p.exists();
        if exists && !force {
            results.push(json!({
                "agent": a,
                "path": p.to_string_lossy().into_owned(),
                "created": false,
                "reason": "already exists (force=true to rotate)",
            }));
            continue;
        }
        let key = os_random_bytes(32)?;
        write_key_file(&p, &hex::encode(&key))?;
        if a == "god" {
            restrict_to_read_only(&p);
        }
        results.push(json!({
            "agent": a,
            "path": p.to_string_lossy().into_owned(),
            "created": true,
            "bytes": 32,
        }));
    }

    Ok(json!({
        "targets": targets,
        "results": results,
        "gitignoreAdded": gitignore_added,
        "authDir": auth_dir().to_string_lossy().into_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_HOME / ANTILOKI_SUBSTRATE_ROOT are process-global; serialize since `cargo
    // test` runs threads in parallel by default, and the same vars are read by other converted
    // twins' tests too — shared crate-wide lock (test_env_lock.rs), same convention as
    // god-driver-halt.rs / byok-key-set.rs.
    fn with_roots<T>(home: &str, substrate_root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", home);
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", substrate_root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-auth-init-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn default_agents_creates_five_keys_and_appends_gitignore() {
        let home = temp_dir("default-home");
        let root = temp_dir("default-root");
        let r = with_roots(home.to_str().unwrap(), root.to_str().unwrap(), || {
            authInit(&json!({}), &db0()).unwrap()
        });
        assert_eq!(r["targets"], json!(["system", "dev", "qa", "man", "god"]));
        assert_eq!(r["gitignoreAdded"], json!(true));
        let results = r["results"].as_array().unwrap();
        assert_eq!(results.len(), 5);
        for entry in results {
            assert_eq!(entry["created"], json!(true));
            assert_eq!(entry["bytes"], json!(32));
            let keys: Vec<&String> = entry.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["agent", "path", "created", "bytes"]);
            let path = entry["path"].as_str().unwrap();
            let hex = fs::read_to_string(path).unwrap();
            assert_eq!(hex.len(), 64);
            assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
                let expected = if entry["agent"] == json!("god") { 0o400 } else { 0o600 };
                assert_eq!(mode, expected);
            }
        }
        let gitignore_text = fs::read_to_string(root.join(".gitignore")).unwrap();
        assert!(gitignore_text.ends_with("# Auth keys for agent identity (never commit)\n.antiloki/auth/\n"));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn second_call_without_force_is_idempotent_and_gitignore_not_duplicated() {
        let home = temp_dir("idempotent-home");
        let root = temp_dir("idempotent-root");
        with_roots(home.to_str().unwrap(), root.to_str().unwrap(), || {
            authInit(&json!({}), &db0()).unwrap()
        });
        let r = with_roots(home.to_str().unwrap(), root.to_str().unwrap(), || {
            authInit(&json!({}), &db0()).unwrap()
        });
        assert_eq!(r["gitignoreAdded"], json!(false));
        for entry in r["results"].as_array().unwrap() {
            assert_eq!(entry["created"], json!(false));
            assert_eq!(entry["reason"], json!("already exists (force=true to rotate)"));
            let keys: Vec<&String> = entry.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["agent", "path", "created", "reason"]);
        }
        let gitignore_text = fs::read_to_string(root.join(".gitignore")).unwrap();
        let count = gitignore_text.split('\n').filter(|l| l.trim() == ".antiloki/auth/").count();
        assert_eq!(count, 1);
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn force_true_rotates_key_with_new_random_bytes() {
        let home = temp_dir("force-home");
        let root = temp_dir("force-root");
        // key_path_str resolves against the live ANTILOKI_HOME env var, so the read must happen
        // INSIDE with_roots too — reading it outside (after with_roots has already unset the
        // override) resolves against the REAL machine home dir instead of the isolated temp dir,
        // silently comparing two reads of a stale, untouched real-machine file.
        let before = with_roots(home.to_str().unwrap(), root.to_str().unwrap(), || {
            authInit(&json!({ "agent": "dev" }), &db0()).unwrap();
            fs::read_to_string(key_path_str("dev")).unwrap()
        });
        let (r, rotated) = with_roots(home.to_str().unwrap(), root.to_str().unwrap(), || {
            let r = authInit(&json!({ "agent": "dev", "force": true }), &db0()).unwrap();
            let rotated = fs::read_to_string(key_path_str("dev")).unwrap();
            (r, rotated)
        });
        assert_eq!(r["results"][0]["created"], json!(true));
        assert_ne!(before, rotated);
        assert_eq!(rotated.len(), 64);
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn single_agent_input_targets_just_that_agent() {
        let home = temp_dir("single-home");
        let root = temp_dir("single-root");
        let r = with_roots(home.to_str().unwrap(), root.to_str().unwrap(), || {
            authInit(&json!({ "agent": "qa" }), &db0()).unwrap()
        });
        assert_eq!(r["targets"], json!(["qa"]));
        assert_eq!(r["results"].as_array().unwrap().len(), 1);
        assert_eq!(r["results"][0]["agent"], json!("qa"));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn agents_array_input_targets_exactly_those_agents() {
        let home = temp_dir("array-home");
        let root = temp_dir("array-root");
        let r = with_roots(home.to_str().unwrap(), root.to_str().unwrap(), || {
            authInit(&json!({ "agents": ["dev", "man"] }), &db0()).unwrap()
        });
        assert_eq!(r["targets"], json!(["dev", "man"]));
        assert_eq!(r["results"].as_array().unwrap().len(), 2);
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_agent_name_returns_exact_quirky_error_message() {
        let home = temp_dir("invalid-home");
        let root = temp_dir("invalid-root");
        let e = with_roots(home.to_str().unwrap(), root.to_str().unwrap(), || {
            authInit(&json!({ "agent": "Bad Name!" }), &db0()).unwrap_err()
        });
        assert_eq!(e, "auth-init: agent must be between [a-z][a-z0-9_-]* and Bad Name! (got undefined)");
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn key_path_resolves_under_auth_dir() {
        let home = temp_dir("keypath-home");
        let expected = home.join("auth").join("dev.key");
        let r = with_roots(home.to_str().unwrap(), home.to_str().unwrap(), || {
            keyPath(&json!("dev"))
        });
        assert_eq!(r, json!(expected.to_string_lossy().into_owned()));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn existing_gitignore_without_trailing_newline_gets_separator() {
        let home = temp_dir("sep-home");
        let root = temp_dir("sep-root");
        fs::write(root.join(".gitignore"), "node_modules/").unwrap();
        with_roots(home.to_str().unwrap(), root.to_str().unwrap(), || {
            authInit(&json!({ "agent": "dev" }), &db0()).unwrap()
        });
        let text = fs::read_to_string(root.join(".gitignore")).unwrap();
        assert_eq!(
            text,
            "node_modules/\n# Auth keys for agent identity (never commit)\n.antiloki/auth/\n"
        );
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }
}
