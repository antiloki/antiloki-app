//! auth-verify.rs — twin of skills/external/auth-verify.mjs. NAMES ARE CONTRACT: `authVerify`
//! (default, the ONLY export — single-unit module per census).
//!
//! Validates an auth token produced by auth-sign: (1) the claimed agent's key file exists on
//! disk, (2) HMAC-SHA256 recomputation matches `sig`, (3) `ts` is within a freshness window.
//! NEVER throws in JS (module docstring: "Never throws — auth failures are data, not
//! exceptions") — every branch returns `{valid,...}` data, so `err.*`/AntilokiError never enters
//! this module.
//!
//! BESPOKE NATIVE, same class as auth-sign.rs's `authSign` / auth-init.rs's `authInit`: the
//! happy path needs a REAL key file at a real-fs-anchored fixed path (`keyPath(agent)`,
//! delegated to the already-converted `external_auth_init::keyPath` twin per the crossing
//! playbook's cross-module-call convention), which the conform-db fixture harness cannot seed
//! (it only seeds SQLite tables, not the filesystem/env). No fixture, no frontier entry — proven
//! below via `#[cfg(test)]`, reusing the `with_home()` env-var-injection pattern from
//! auth-sign.rs / auth-init.rs (shared `crate::test_env_lock::ENV_LOCK`).
//!
//! `Date.now()` (the freshness check) is genuinely real wall-clock time here, NOT the crossing
//! playbook's frozen-clock literal — same reasoning as auth-sign.rs's `ts` field: this module has
//! no fixture to freeze against.
//!
//! HMAC: `computeSig`'s real production call passes a raw hex-decoded `Buffer` as `key`, not a
//! JSON value — so this twin does NOT call `external_auth_sign::computeSig` (that fixtured helper
//! models `key` as a JSON-string-via-`str_qq`, correct for ITS OWN fixture but wrong for a raw
//! binary key here). Instead it copies the same private `hmac_sha256_hex` byte-level helper
//! auth-sign.rs uses internally, applied to the real decoded key bytes — matching what
//! `createHmac('sha256', key)` does for the real `Buffer` key `authVerify` always receives.
//!
//! `key`/`sig` hex decoding mirrors `Buffer.from(str, 'hex')`, which — unlike `hex::decode` —
//! never throws: it decodes valid hex-digit pairs from the start and stops silently at the first
//! invalid byte (a truncated buffer) instead of erroring on the whole string. Modeled by
//! `buffer_from_hex_lenient`. Not fixture-exercised (no test constructs malformed-hex input),
//! disclosed as a narrow gap, same class as auth-init.rs's non-string-agent gap.
//!
//! `timingSafeEqual` is modeled as a plain constant-time-shaped byte compare (`ct_eq`) — this
//! twin is judged on OUTPUT equality only, never on real side-channel timing (a literal
//! `Vec<u8>::eq` would be observably identical for every input this module can ever produce); the
//! constant-time shape is kept anyway to mirror the source's intent for a reader, not because
//! conform requires it.
//!
//! GOD mode: `process.stdin?.isTTY` has no Rust equivalent reachable from this pure fn (and every
//! process that runs this twin — `cargo test`, the conform harness — is non-interactive anyway,
//! same as the JS lane under `node --test`), so `ttyOk` is modeled as always `false`; only
//! `ANTILOKI_GOD_AUTHORIZED=true` can unblock the `god` branch here, matching what both lanes
//! actually observe in a non-interactive process per law GOD_MODE_TTY_REQUIRED.
//!
//! Throw fidelity: N/A (never throws). A real fs read error after the `existsSync` check would
//! propagate as an uncaught JS exception; this twin surfaces the equivalent as `Err(String)` from
//! `std::fs::read_to_string`, matching the same uncaught-error class as auth-sign.rs's `authSign`.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::PathBuf;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }

fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); }
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k { format!("{digits}{}", "0".repeat((exp + 1 - k) as usize)) }
            else { let (i, f) = digits.split_at((exp + 1) as usize); format!("{i}.{f}") }
        } else { format!("0.{}{digits}", "0".repeat((-exp - 1) as usize)) }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}

fn is_nullish(v: Option<&Value>) -> bool { matches!(v, None | Some(Value::Null)) } // JS `x == null`

fn now_epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

// Byte-level HMAC-SHA256, applied to the REAL decoded key bytes (see module doc for why this
// isn't routed through external_auth_sign::computeSig's JSON-Value-shaped helper).
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

// = `Buffer.from(s, 'hex')`: decode hex-digit pairs from the start, stop silently (truncated, not
// an error) at the first invalid pair — unlike `hex::decode`, which errors on the whole string.
fn buffer_from_hex_lenient(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16);
        let lo = (bytes[i + 1] as char).to_digit(16);
        match (hi, lo) {
            (Some(h), Some(l)) => { out.push(((h << 4) | l) as u8); i += 2; }
            _ => break,
        }
    }
    out
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for i in 0..a.len() { diff |= a[i] ^ b[i]; }
    diff == 0
}

const DEFAULT_FRESHNESS_MS: f64 = 300_000.0; // 5 * 60 * 1000

/// `export default function authVerify(input, _context)` — `_context` is unused (JS destructures
/// only `input`), mirrored by the unused `_db` param below.
pub fn authVerify(input: &Value, _db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let agent_v = obj.and_then(|o| o.get("agent"));
    let nonce_v = obj.and_then(|o| o.get("nonce"));
    let ts_v = obj.and_then(|o| o.get("ts"));
    let sig_v = obj.and_then(|o| o.get("sig"));
    let freshness_v = obj.and_then(|o| o.get("freshness_ms"));

    if !truthy(agent_v) || is_nullish(nonce_v) || is_nullish(ts_v) || !truthy(sig_v) {
        return Ok(json!({
            "valid": false,
            "reason": "auth token missing required fields (agent/nonce/ts/sig)",
        }));
    }
    let agent_val = agent_v.unwrap();
    let agent_str = tmpl(Some(agent_val)); // `${agent}` in keyPath/message building below

    let key_path_val = crate::external_auth_init::keyPath(agent_val);
    let p = PathBuf::from(key_path_val.as_str().unwrap_or_default());
    if !p.exists() {
        return Ok(json!({
            "valid": false,
            "reason": format!("no auth key for agent '{agent_str}'"),
        }));
    }

    // Freshness — reject stale tokens to prevent replay
    let age = now_epoch_ms() as f64 - js_number(ts_v);
    let freshness_ms = match freshness_v {
        None => DEFAULT_FRESHNESS_MS, // JS default parameter only fires when the key is absent
        Some(v) => js_number(Some(v)),
    };
    if age < -10_000.0 || age > freshness_ms {
        return Ok(json!({
            "valid": false,
            "reason": format!(
                "token age {}ms outside freshness window {}ms",
                js_number_to_string(age),
                js_number_to_string(freshness_ms)
            ),
        }));
    }

    // HMAC recompute + constant-time compare
    let key_hex = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    let key = buffer_from_hex_lenient(key_hex.trim());
    let nonce_str = tmpl(nonce_v);
    let ts_num_str = js_number_to_string(js_number(ts_v)); // `Number(ts)` interpolated in computeSig
    let message = format!("{agent_str}|{nonce_str}|{ts_num_str}");
    let expected_hex = hmac_sha256_hex(&key, message.as_bytes());
    let expected_buf = buffer_from_hex_lenient(&expected_hex);
    let got_buf = buffer_from_hex_lenient(&tmpl(sig_v));
    if expected_buf.len() != got_buf.len() || !ct_eq(&expected_buf, &got_buf) {
        return Ok(json!({ "valid": false, "reason": "signature mismatch" }));
    }

    // GOD mode requires TTY OR explicit authorization env var (see module doc: ttyOk always
    // false in this non-interactive twin).
    if agent_str == "god" {
        let env_ok = std::env::var("ANTILOKI_GOD_AUTHORIZED").map(|v| v == "true").unwrap_or(false);
        if !env_ok {
            return Ok(json!({
                "valid": false,
                "reason": "GOD mode requires interactive TTY or ANTILOKI_GOD_AUTHORIZED=true (AI subprocesses blocked)",
            }));
        }
    }

    Ok(json!({ "valid": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // SAFETY: ANTILOKI_HOME / ANTILOKI_GOD_AUTHORIZED are process-global; serialize since `cargo
    // test` runs threads in parallel by default — shared crate-wide lock (test_env_lock.rs), same
    // convention as auth-init.rs / auth-sign.rs.
    fn with_home<T>(antiloki_home: &str, god_authorized: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
            match god_authorized {
                Some(v) => std::env::set_var("ANTILOKI_GOD_AUTHORIZED", v),
                None => std::env::remove_var("ANTILOKI_GOD_AUTHORIZED"),
            }
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
            std::env::remove_var("ANTILOKI_GOD_AUTHORIZED");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-auth-verify-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    const KEY_HEX: &str = "deadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00ddeadbeefcafef00d";

    fn write_key(home: &PathBuf, agent: &str) {
        let auth_dir = home.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(auth_dir.join(format!("{agent}.key")), format!("{KEY_HEX}\n")).unwrap();
    }

    fn token(agent: &str, nonce: &str, ts: i64) -> Value {
        let key = hex::decode(KEY_HEX).unwrap();
        let message = format!("{agent}|{nonce}|{ts}");
        let sig = hmac_sha256_hex(&key, message.as_bytes());
        json!({ "agent": agent, "nonce": nonce, "ts": ts, "sig": sig })
    }

    #[test]
    fn missing_required_fields_returns_invalid() {
        let r = authVerify(&json!({}), &db0()).unwrap();
        assert_eq!(r, json!({ "valid": false, "reason": "auth token missing required fields (agent/nonce/ts/sig)" }));
        let r2 = authVerify(&json!({ "agent": "dev", "nonce": "n", "ts": 1 }), &db0()).unwrap();
        assert_eq!(r2["valid"], json!(false));
    }

    #[test]
    fn no_key_file_for_agent_returns_invalid() {
        let home = temp_dir("no-key");
        let r = with_home(home.to_str().unwrap(), None, || {
            authVerify(&token("nosuchagent", "n", now_epoch_ms()), &db0()).unwrap()
        });
        assert_eq!(r, json!({ "valid": false, "reason": "no auth key for agent 'nosuchagent'" }));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn stale_token_returns_invalid_with_exact_reason() {
        let home = temp_dir("stale");
        write_key(&home, "dev");
        let ts = now_epoch_ms() - 400_000;
        let r = with_home(home.to_str().unwrap(), None, || {
            authVerify(&token("dev", "n1", ts), &db0()).unwrap()
        });
        assert_eq!(r["valid"], json!(false));
        assert!(r["reason"].as_str().unwrap().starts_with("token age "));
        assert!(r["reason"].as_str().unwrap().ends_with("ms outside freshness window 300000ms"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn future_token_beyond_grace_returns_invalid() {
        let home = temp_dir("future");
        write_key(&home, "dev");
        let ts = now_epoch_ms() + 20_000;
        let r = with_home(home.to_str().unwrap(), None, || {
            authVerify(&token("dev", "n1", ts), &db0()).unwrap()
        });
        assert_eq!(r["valid"], json!(false));
        assert!(r["reason"].as_str().unwrap().contains("outside freshness window"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn custom_freshness_ms_widens_window() {
        let home = temp_dir("custom-freshness");
        write_key(&home, "dev");
        let ts = now_epoch_ms() - 400_000;
        let mut t = token("dev", "n1", ts);
        t.as_object_mut().unwrap().insert("freshness_ms".into(), json!(1_000_000));
        let r = with_home(home.to_str().unwrap(), None, || authVerify(&t, &db0()).unwrap());
        assert_eq!(r, json!({ "valid": true }));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn signature_mismatch_returns_invalid() {
        let home = temp_dir("mismatch");
        write_key(&home, "dev");
        let mut t = token("dev", "n1", now_epoch_ms());
        t["sig"] = json!("00".repeat(32));
        let r = with_home(home.to_str().unwrap(), None, || authVerify(&t, &db0()).unwrap());
        assert_eq!(r, json!({ "valid": false, "reason": "signature mismatch" }));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn valid_non_god_token_returns_valid_true() {
        let home = temp_dir("valid");
        write_key(&home, "dev");
        let t = token("dev", "n1", now_epoch_ms());
        let r = with_home(home.to_str().unwrap(), None, || authVerify(&t, &db0()).unwrap());
        assert_eq!(r, json!({ "valid": true }));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn god_agent_without_authorization_is_blocked() {
        let home = temp_dir("god-blocked");
        write_key(&home, "god");
        let t = token("god", "n1", now_epoch_ms());
        let r = with_home(home.to_str().unwrap(), None, || authVerify(&t, &db0()).unwrap());
        assert_eq!(r, json!({
            "valid": false,
            "reason": "GOD mode requires interactive TTY or ANTILOKI_GOD_AUTHORIZED=true (AI subprocesses blocked)"
        }));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn god_agent_with_env_authorization_is_valid() {
        let home = temp_dir("god-authorized");
        write_key(&home, "god");
        let t = token("god", "n1", now_epoch_ms());
        let r = with_home(home.to_str().unwrap(), Some("true"), || authVerify(&t, &db0()).unwrap());
        assert_eq!(r, json!({ "valid": true }));
        let _ = fs::remove_dir_all(&home);
    }
}
