//! god-override-verify.rs — twin of skills/agents/god/god-override-verify.mjs. NAMES ARE
//! CONTRACT: `godOverrideVerify` (default, standard db-lane signature though `_db` is unused,
//! matching the JS `(input, _context)` shape), `parseChallenge` (named export, pure no-db
//! helper), `PUBKEY_PATH` (the roots.mjs-derived constant re-export — not a census unit; modeled
//! here as the `pubkey_path()` fn, same convention as the sibling god-driver-halt.rs /
//! god-driver-resume.rs HALT_FILE re-export and tui-god-token.rs TOKEN_HASH_FILE re-export).
//!
//! BESPOKE-NATIVE (godOverrideVerify only): verifies a real Ed25519 signature — node:crypto
//! `verify(null, Buffer.from(challenge,'utf8'), publicKey, sigBytes)` — against a real SPKI PEM
//! public key file at roots.mjs AUTH_DIR/god_pub.pem. AUTH_DIR = HOME_DIR/auth, HOME_DIR =
//! $ANTILOKI_HOME or homedir()/.antiloki — a HOME-anchored, NOT db-derived, root (same class as
//! god-driver-halt.rs / god-driver-resume.rs's HALT_FILE and tui-god-token.rs's
//! TOKEN_HASH_FILE). No conform-db fixture-lane exists for real fs+crypto I/O against that root,
//! so per the playbook this fn is bespoke-native: no fixture, no frontier entry, no dispatch
//! arm — proven instead by the #[cfg(test)] native tests below (temp dir + ANTILOKI_HOME env-var
//! injection, copying the `with_home()` pattern from god-driver-halt.rs, using a real generated
//! Ed25519 keypair).
//!
//! `parseChallenge` has none of that (pure regex over a string, no fs/db/crypto) — it IS
//! conform-db fixture-tested via the no-db helper lane; see
//! fixtures/agents_god_god_override_verify__parse_challenge.json.
//!
//! Frozen clock: `Date.now()` → 1767225600000 (2026-01-01T00:00:00.000Z, the playbook's frozen
//! instant) drives the freshness/`age_ms` computation. `Date.parse(parsed.ts)` is twinned with a
//! hand-rolled ISO-8601 → epoch-ms parser (same technique as skills/system/drift-list.rs's
//! `parse_iso_ms`) since the challenge format's `ts` segment is documented to be an ISO 8601
//! timestamp and no chrono-class crate is in the dependency set.
//!
//! Ed25519: no pkcs8/pem parsing crate is added — node:crypto's SPKI PEM export for an Ed25519
//! key is a FIXED 44-byte DER shape (12-byte constant header for OID 1.3.101.112 + the 32 raw
//! public-key bytes), so the PEM is stripped/base64-decoded by hand (the `base64` crate is
//! already a dependency) and the trailing 32 bytes feed straight into
//! `ed25519_dalek::VerifyingKey::from_bytes`. Mirrors the sha2+hex precedent
//! (tui-god-token.rs / codebase-file-read.rs) of adding one narrow, precise crate for a real
//! node:crypto primitive rather than hand-rolling the algorithm itself — `ed25519-dalek = "2"`
//! was added to Cargo.toml for this twin.
//!
//! Error paths: every JS throw (`err.required`, `AntilokiError`) becomes `Err(String)` formatted
//! as `"god-override-verify: <cause>"` (mirrors AntilokiError's `${skill}: ${cause}` message
//! join, same convention as god-card-driver-intent-set.rs's guard-throw messages) — this fn has
//! no fixture lane at all, so these are disclosed-best-effort text, not byte-contracted. The
//! `freshness_ms` default/`null` distinction (JS destructure default fires only on a MISSING
//! key; an explicit `null` falls through to `Number(null)===0` in the relational compare) is
//! folded into one branch here for simplicity — an unrealistic input no real caller sends.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::OnceLock;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use regex::Regex;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

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
// AUTH_DIR = join(HOME_DIR, 'auth'); PUBKEY_PATH = resolve(AUTH_DIR, 'god_pub.pem').
fn pubkey_path() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("auth").join("god_pub.pem")
}

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's
// algorithm (same as skills/system/drift-list.rs's days_from_civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// Parse 'YYYY-MM-DD[T| ]HH:MM:SS[.sss][Z]' as a UTC instant → epoch ms (mirrors Date.parse of an
// ISO 8601 string — the documented shape of a challenge's `ts` segment). Malformed → None (JS
// NaN, where `!Number.isFinite` is true). Same technique as skills/system/drift-list.rs's
// `parse_iso_ms`.
fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' { return None; }
    let num = |lo: usize, hi: usize| -> Option<i64> {
        let sub = s.get(lo..hi)?;
        if sub.bytes().all(|c| c.is_ascii_digit()) { sub.parse::<i64>().ok() } else { None }
    };
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let (mut hh, mut mm, mut ss, mut ms) = (0i64, 0i64, 0i64, 0i64);
    if b.len() > 10 {
        if b.len() < 19 || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' { return None; }
        hh = num(11, 13)?;
        mm = num(14, 16)?;
        ss = num(17, 19)?;
        if b.len() > 19 && b[19] == b'.' {
            let mut f3: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
            while f3.len() < 3 { f3.push('0'); }
            ms = f3.parse::<i64>().unwrap_or(0);
        }
    }
    Some((((days_from_civil(y, mo, d) * 24 + hh) * 60 + mm) * 60 + ss) * 1000 + ms)
}

const DEFAULT_FRESHNESS_MS: i64 = 5 * 60 * 1000;

fn challenge_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // JS: /^antiloki-god-override:([^:]+):(.+)$/
    RE.get_or_init(|| Regex::new(r"^antiloki-god-override:([^:]+):(.+)$").unwrap())
}

fn parse_challenge_inner(challenge: &str) -> Option<(String, String)> {
    let caps = challenge_re().captures(challenge)?;
    Some((caps[1].to_string(), caps[2].to_string()))
}

pub fn parseChallenge(challenge: &Value) -> Value {
    let s = str_qq(Some(challenge)); // String(challenge ?? '')
    match parse_challenge_inner(&s) {
        Some((scope, ts)) => json!({ "scope": scope, "ts": ts }),
        None => Value::Null,
    }
}

// SPKI DER prefix for an Ed25519 (id-Ed25519, RFC 8410) public key: SEQUENCE{ SEQUENCE{ OID
// 1.3.101.112 }, BIT STRING(33, unused=0, <32 raw bytes>) } — 12 fixed bytes + the 32 raw key
// bytes = 44 total. This is exactly what node:crypto's `createPublicKey(pem).export({type:
// 'spki', format:'pem'})` emits for an Ed25519 key (no algorithm parameters, so the shape never
// varies) — avoids pulling in a pkcs8/asn1 parser crate for one fixed-shape structure.
const ED25519_SPKI_PREFIX: [u8; 12] = [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];

fn strip_pem_body(pem: &str) -> Option<String> {
    let mut body = String::new();
    let mut in_block = false;
    for line in pem.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN") { in_block = true; continue; }
        if line.starts_with("-----END") { in_block = false; continue; }
        if in_block { body.push_str(line); }
    }
    if body.is_empty() { None } else { Some(body) }
}

fn verifying_key_from_pem(pem: &str) -> Result<VerifyingKey, String> {
    use base64::Engine;
    let b64 = strip_pem_body(pem).ok_or_else(|| "public key PEM has no body".to_string())?;
    let der = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("public key PEM is not valid base64: {e}"))?;
    if der.len() != 44 || &der[..12] != ED25519_SPKI_PREFIX.as_slice() {
        return Err("public key is not a recognized Ed25519 SPKI DER structure".to_string());
    }
    let mut raw = [0u8; 32];
    raw.copy_from_slice(&der[12..]);
    VerifyingKey::from_bytes(&raw).map_err(|e| format!("invalid Ed25519 public key bytes: {e}"))
}

pub fn godOverrideVerify(input: &Value, _db: &Connection) -> Result<Value, String> {
    let challenge_v = input.get("challenge").cloned().unwrap_or(Value::Null);
    let signature_hex_v = input.get("signature_hex").cloned().unwrap_or(Value::Null);
    let freshness_ms: i64 = match input.get("freshness_ms") {
        None | Some(Value::Null) => DEFAULT_FRESHNESS_MS,
        Some(v) => js_number(Some(v)) as i64,
    };

    if !truthy(Some(&challenge_v)) {
        return Err("god-override-verify: challenge is required".into());
    }
    if !truthy(Some(&signature_hex_v)) {
        return Err("god-override-verify: signature_hex is required".into());
    }

    let challenge_str = str_qq(Some(&challenge_v));
    let (scope, ts) = match parse_challenge_inner(&challenge_str) {
        Some(pair) => pair,
        None => {
            return Err(
                "god-override-verify: challenge must match \"antiloki-god-override:<scope>:<iso8601>\""
                    .into(),
            )
        }
    };

    let ts_ms = match parse_iso_ms(&ts) {
        Some(ms) => ms,
        None => {
            return Err(format!(
                "god-override-verify: challenge timestamp '{ts}' is not a valid ISO 8601"
            ))
        }
    };

    let age = crate::clock::now_ms() - ts_ms;
    if age > freshness_ms || age < -10_000 {
        return Err(format!(
            "god-override-verify: signed challenge is too old: age_ms={age}, freshness_ms={freshness_ms}. Re-sign and retry."
        ));
    }

    let path = pubkey_path();
    if !path.exists() {
        return Err(format!(
            "god-override-verify: god public key not found at {}. Generate a keypair and save the public half there.",
            path.to_string_lossy()
        ));
    }
    let pem = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "god-override-verify: failed reading public key at {}: {e}",
            path.to_string_lossy()
        )
    })?;
    let verifying_key = verifying_key_from_pem(&pem).map_err(|e| format!("god-override-verify: {e}"))?;

    let signature_hex_str = str_qq(Some(&signature_hex_v)); // String(signature_hex)
    let sig_bytes = match hex::decode(signature_hex_str.trim()) {
        Ok(b) => b,
        Err(e) => return Err(format!("god-override-verify: signature_hex is not valid hex: {e}")),
    };
    if sig_bytes.is_empty() {
        return Err("god-override-verify: signature_hex decoded to zero bytes".into());
    }

    let ok = match Signature::try_from(sig_bytes.as_slice()) {
        Ok(signature) => verifying_key.verify(challenge_str.as_bytes(), &signature).is_ok(),
        Err(_) => false,
    };
    if !ok {
        return Err(
            "god-override-verify: signature did not verify against god public key (wrong key, tampered challenge, or wrong signature)"
                .into(),
        );
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("scope".into(), json!(scope));
    out.insert("signed_at".into(), json!(ts));
    out.insert("age_ms".into(), json!(age));
    out.insert("pubkey_path".into(), json!(path.to_string_lossy().into_owned()));
    out.insert(
        "advice".into(),
        json!("Signature valid. Now perform the gated action; the caller decides what is unlocked."),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::fs;

    // SAFETY: ANTILOKI_HOME is process-global; serialize via the shared crate-wide lock — a
    // local mutex here would not prevent a race with OTHER files' tests over the same real env
    // var (see test_env_lock.rs).
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
        dir.push(format!("antiloki-god-override-verify-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    fn test_signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn write_pubkey(dir: &std::path::Path, vk: &VerifyingKey) {
        use base64::Engine;
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        let mut der = ED25519_SPKI_PREFIX.to_vec();
        der.extend_from_slice(vk.as_bytes());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----\n");
        fs::write(auth_dir.join("god_pub.pem"), pem).unwrap();
    }

    fn sign_hex(sk: &SigningKey, challenge: &str) -> String {
        let sig: Signature = sk.sign(challenge.as_bytes());
        hex::encode(sig.to_bytes())
    }

    #[test]
    fn parse_challenge_well_formed() {
        assert_eq!(
            parseChallenge(&json!("antiloki-god-override:DELETE-CARD:2026-05-21T00:00:00.000Z")),
            json!({ "scope": "DELETE-CARD", "ts": "2026-05-21T00:00:00.000Z" })
        );
    }

    #[test]
    fn parse_challenge_bad_shape() {
        assert_eq!(parseChallenge(&json!("not a challenge")), Value::Null);
        assert_eq!(parseChallenge(&json!("")), Value::Null);
        assert_eq!(parseChallenge(&Value::Null), Value::Null);
    }

    #[test]
    fn valid_signed_challenge_verifies() {
        let dir = temp_dir("valid");
        let sk = test_signing_key();
        write_pubkey(&dir, &sk.verifying_key());
        let challenge = "antiloki-god-override:TEST-SCOPE:2026-01-01T00:00:00.000Z";
        let sig = sign_hex(&sk, challenge);
        let r = with_home(dir.to_str().unwrap(), || {
            godOverrideVerify(&json!({ "challenge": challenge, "signature_hex": sig }), &db0())
        })
        .unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["scope"], json!("TEST-SCOPE"));
        assert_eq!(r["signed_at"], json!("2026-01-01T00:00:00.000Z"));
        assert_eq!(r["age_ms"], json!(0));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "scope", "signed_at", "age_ms", "pubkey_path", "advice"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tampered_signature_fails() {
        let dir = temp_dir("tampered");
        let sk = test_signing_key();
        write_pubkey(&dir, &sk.verifying_key());
        let challenge = "antiloki-god-override:T:2026-01-01T00:00:00.000Z";
        let mut sig = sign_hex(&sk, challenge);
        let last = sig.pop().unwrap();
        sig.push(if last == '0' { '1' } else { '0' });
        let r = with_home(dir.to_str().unwrap(), || {
            godOverrideVerify(&json!({ "challenge": challenge, "signature_hex": sig }), &db0())
        });
        assert!(r.unwrap_err().contains("did not verify"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_signature_rejected() {
        let dir = temp_dir("stale");
        let sk = test_signing_key();
        write_pubkey(&dir, &sk.verifying_key());
        // well over an hour before the frozen crate::clock::now_ms() (2026-01-01T00:00:00.000Z)
        let challenge = "antiloki-god-override:T:2025-12-01T00:00:00.000Z";
        let sig = sign_hex(&sk, challenge);
        let r = with_home(dir.to_str().unwrap(), || {
            godOverrideVerify(&json!({ "challenge": challenge, "signature_hex": sig }), &db0())
        });
        assert!(r.unwrap_err().contains("too old"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_challenge_rejected() {
        let dir = temp_dir("malformed");
        let sk = test_signing_key();
        write_pubkey(&dir, &sk.verifying_key());
        let sig = sign_hex(&sk, "whatever");
        let r = with_home(dir.to_str().unwrap(), || {
            godOverrideVerify(&json!({ "challenge": "whatever", "signature_hex": sig }), &db0())
        });
        assert!(r.unwrap_err().contains("must match"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_pubkey_reports_not_found() {
        let dir = temp_dir("missing-pubkey");
        let challenge = "antiloki-god-override:T:2026-01-01T00:00:00.000Z";
        let r = with_home(dir.to_str().unwrap(), || {
            godOverrideVerify(&json!({ "challenge": challenge, "signature_hex": "ab" }), &db0())
        });
        assert!(r.unwrap_err().contains("god public key not found"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_challenge_or_signature_rejected() {
        let c = db0();
        assert!(godOverrideVerify(&json!({ "signature_hex": "ab" }), &c).is_err());
        assert!(godOverrideVerify(&json!({ "challenge": "x" }), &c).is_err());
    }
}
