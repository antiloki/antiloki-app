//! auth-sign.rs — twin of skills/external/auth-sign.mjs. NAMES ARE CONTRACT: `authSign`
//! (default), `computeSig` (named export).
//!
//! MIXED module, same split as driver-token.rs / auth-init.rs:
//! - `computeSig(key, agent, nonce, ts)` is pure computation (HMAC-SHA256 over
//!   `${agent}|${nonce}|${ts}` keyed by `key`) — fixture-tested as an ordinary `sig:"no-db"`
//!   helper. `key` is whatever JSON value the caller passes; production always passes a
//!   `Buffer` (hex-decoded key bytes), but the pure fn itself never decodes — it just uses
//!   `key`'s bytes as the HMAC key exactly like `crypto.createHmac('sha256', key)` would for
//!   whatever value flows in. Modeled here as a JSON string used via its raw UTF-8 bytes,
//!   matching what `createHmac` does when handed a JS string key (no hex-decoding inside this
//!   fn — that only happens in `authSign` below, one level up).
//! - `authSign(input, _context)` (the default export) reads a REAL key file at a fixed-constant
//!   path (`keyPath(agent)`, delegated to the already-converted `external_auth_init::keyPath`
//!   twin per the crossing playbook's cross-module-call convention) and draws `randomBytes(8)`
//!   for the nonce, which is itself part of the observable output (`nonce`, and `sig` derived
//!   from it) — genuinely non-deterministic, same class as auth-init.rs's `authInit` /
//!   driver-token.rs's `loadOrCreateDriverToken`. BESPOKE NATIVE TESTING only (see manifest: no
//!   fixture, no frontier entry for `authSign`), proven below via `#[cfg(test)]`, reusing the
//!   `with_home()` env-var-injection pattern from auth-init.rs / driver-token.rs.
//!
//! Randomness (`randomBytes(8)`): the same zero-new-Cargo-dependency OS CSPRNG reach-in as
//! auth-init.rs / driver-token.rs — `/dev/urandom` on unix, `RtlGenRandom` (advapi32.dll
//! `SystemFunction036`) via a minimal `extern "system"` FFI on Windows.
//!
//! `Date.now()` (the `ts` field) is genuinely real wall-clock time here, NOT the crossing
//! playbook's frozen-clock literal — that convention only pins the JS/Rust conform-db LANES to
//! the same instant for fixture comparison; `authSign` has no fixture (nonce already makes the
//! whole output non-reproducible), so there is nothing to freeze against. The twin below calls
//! real `SystemTime::now()`.
//!
//! Throw fidelity (verified against real JS via `node --test skills/external/auth-sign.test.mjs`):
//! - missing `agent` → `err.required('auth-sign', 'agent')` → `"auth-sign: agent is required"`.
//! - no key file on disk → `AntilokiError{code:CONFIG_MISSING, skill:'auth-sign', cause:"auth key
//!   not found for agent '<agent>' (run system.auth.init first)"}` → message
//!   `"auth-sign: auth key not found for agent '<agent>' (run system.auth.init first)"`.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::PathBuf;

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

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

fn now_epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let hashed = Sha256::digest(key);
        key_block[..32].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    hex::encode(outer.finalize())
}

/// `function computeSig(key, agent, nonce, ts) { return createHmac('sha256', key)
/// .update(`${agent}|${nonce}|${ts}`).digest('hex'); }`
///
/// `key` is used via its raw bytes exactly like `crypto.createHmac` does for whatever JS value
/// flows in — no hex-decoding here (that belongs to the caller, see module doc).
pub fn computeSig(key: &Value, agent: &Value, nonce: &Value, ts: &Value) -> Value {
    let key_bytes = str_qq(Some(key));
    let message = format!("{}|{}|{}", str_qq(Some(agent)), str_qq(Some(nonce)), str_qq(Some(ts)));
    json!(hmac_sha256_hex(key_bytes.as_bytes(), message.as_bytes()))
}

/// `export default function authSign(input, _context)` — `_context` is unused (JS destructures
/// only `input`), mirrored by the unused `_db` param below.
pub fn authSign(input: &Value, _db: &Connection) -> Result<Value, String> {
    let agent = input.as_object().and_then(|o| o.get("agent")).and_then(Value::as_str);
    let agent = match agent {
        Some(a) if !a.is_empty() => a,
        _ => return Err("auth-sign: agent is required".to_string()),
    };

    let key_path_val = crate::external_auth_init::keyPath(&json!(agent));
    let p = PathBuf::from(key_path_val.as_str().unwrap_or_default());
    if !p.exists() {
        return Err(format!(
            "auth-sign: auth key not found for agent '{agent}' (run system.auth.init first)"
        ));
    }

    let key_hex = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    let key_hex = key_hex.trim();
    let key = hex::decode(key_hex).map_err(|e| e.to_string())?;
    let nonce_bytes = os_random_bytes(8)?;
    let nonce = hex::encode(nonce_bytes);
    let ts = now_epoch_ms();
    let sig_hex = hmac_sha256_hex(&key, format!("{agent}|{nonce}|{ts}").as_bytes());

    Ok(json!({
        "agent": agent,
        "nonce": nonce,
        "ts": ts,
        "sig": sig_hex,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default, and this same var is read by other converted twins' tests too —
    // shared crate-wide lock (test_env_lock.rs), same convention as auth-init.rs /
    // driver-token.rs.
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
        dir.push(format!("antiloki-auth-sign-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn compute_sig_matches_manual_hmac_vector() {
        let key = json!("deadbeefcafef00d");
        let sig = computeSig(&key, &json!("dev"), &json!("abc123"), &json!(1767225600000i64));
        let expected = hmac_sha256_hex(b"deadbeefcafef00d", b"dev|abc123|1767225600000");
        assert_eq!(sig, json!(expected));
        assert_eq!(sig.as_str().unwrap().len(), 64);
    }

    #[test]
    fn compute_sig_changes_with_each_input() {
        let key = json!("k");
        let base = computeSig(&key, &json!("dev"), &json!("n1"), &json!(1000));
        assert_ne!(computeSig(&key, &json!("qa"), &json!("n1"), &json!(1000)), base);
        assert_ne!(computeSig(&key, &json!("dev"), &json!("n2"), &json!(1000)), base);
        assert_ne!(computeSig(&key, &json!("dev"), &json!("n1"), &json!(2000)), base);
    }

    #[test]
    fn auth_sign_missing_agent_returns_exact_message() {
        let e = authSign(&json!({}), &db0()).unwrap_err();
        assert_eq!(e, "auth-sign: agent is required");
    }

    #[test]
    fn auth_sign_no_key_file_returns_exact_config_missing_message() {
        let dir = temp_dir("missing-key");
        let e = with_home(dir.to_str().unwrap(), || {
            authSign(&json!({ "agent": "nosuchagent" }), &db0()).unwrap_err()
        });
        assert_eq!(
            e,
            "auth-sign: auth key not found for agent 'nosuchagent' (run system.auth.init first)"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auth_sign_reads_key_file_and_returns_verifiable_signature() {
        let dir = temp_dir("sign");
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        let key_hex = "a1b2c3d4".repeat(8);
        fs::write(auth_dir.join("dev.key"), format!("{key_hex}\n")).unwrap();
        let r = with_home(dir.to_str().unwrap(), || {
            authSign(&json!({ "agent": "dev" }), &db0()).unwrap()
        });
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["agent", "nonce", "ts", "sig"]);
        assert_eq!(r["agent"], json!("dev"));
        let nonce = r["nonce"].as_str().unwrap();
        assert_eq!(nonce.len(), 16);
        assert!(nonce.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(r["ts"].is_i64() || r["ts"].is_u64());
        let sig = r["sig"].as_str().unwrap();
        assert_eq!(sig.len(), 64);
        let key = hex::decode(&key_hex).unwrap();
        let expected = hmac_sha256_hex(&key, format!("dev|{}|{}", nonce, r["ts"]).as_bytes());
        assert_eq!(sig, expected);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn auth_sign_two_calls_produce_different_nonces() {
        let dir = temp_dir("two-calls");
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(auth_dir.join("qa.key"), "ff".repeat(32)).unwrap();
        let (r1, r2) = with_home(dir.to_str().unwrap(), || {
            let r1 = authSign(&json!({ "agent": "qa" }), &db0()).unwrap();
            let r2 = authSign(&json!({ "agent": "qa" }), &db0()).unwrap();
            (r1, r2)
        });
        assert_ne!(r1["nonce"], r2["nonce"]);
        assert_ne!(r1["sig"], r2["sig"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
