//! driver-token.rs — twin of skills/audit/driver-token.mjs. NAMES ARE CONTRACT: `driverToken`
//! (default), `loadOrCreateDriverToken`, `readDriverToken`, `hashInput`, `signDriverCall`,
//! `verifyDriverCall`. `DEFAULT_FRESHNESS_MS` is a plain numeric const re-export (not a function —
//! not a census unit per the crossing playbook), modeled as a private Rust `const`.
//!
//! MIXED module. Three units touch a REAL file at a fixed-constant path — roots.mjs `AUTH_DIR` =
//! `HOME_DIR/auth`, never DB-derived, same class as auth-init.rs / tui-god-token.rs /
//! god-driver-halt.rs — so `loadOrCreateDriverToken`, `readDriverToken`, and the default export
//! `driverToken` (which just calls `loadOrCreateDriverToken`) are BESPOKE NATIVE: proven only by
//! `#[cfg(test)]`, no fixture / no frontier entry (see manifest — those 3 units are left out of
//! arms/frontier per the crossing playbook's bespoke-native carve-out). The other three units —
//! `hashInput`, `signDriverCall`, `verifyDriverCall` — are pure computation (no fs, no db) and ARE
//! fixture-tested as ordinary `sig:"no-db"` helpers.
//!
//! HOME_DIR/AUTH_DIR resolution mirrors auth-init.rs / tui-god-token.rs / god-driver-halt.rs (no
//! `dirs`-crate dependency): `ANTILOKI_HOME` env override (plain path join, no cwd-resolve —
//! callers never pass a relative value in practice) else Node `os.homedir()` semantics (POSIX
//! `$HOME`; Windows `%USERPROFILE%` falling back to `%HOMEDRIVE%%HOMEPATH%`).
//!
//! Randomness (`loadOrCreateDriverToken`'s `randomBytes(32)`): the same zero-new-Cargo-dependency
//! OS CSPRNG reach-in as auth-init.rs — `/dev/urandom` on unix, `RtlGenRandom` (advapi32.dll
//! `SystemFunction036`) via a minimal `extern "system"` FFI on Windows.
//!
//! HMAC-SHA256 (`signDriverCall` / `verifyDriverCall`): hand-rolled per RFC 2104 directly on top
//! of the already-present `sha2` dependency (no `hmac` crate) — same zero-new-dependency posture
//! as the OS-random reach-in above.
//!
//! `hashInput`'s `JSON.stringify(input ?? null, Object.keys(input ?? {}).sort())` uses the
//! replacer-ARRAY form of `JSON.stringify`: an allow-list of key names applied RECURSIVELY at
//! every object level (verified against real Node — a nested object's own differently-named keys
//! are dropped unless they happen to share a name with a top-level key), with output key ORDER
//! following the array's (sorted) order, not the source object's insertion order. Array elements
//! are never filtered by index — only nested objects reachable through them are. Modeled below as
//! `js_own_keys` + `json_filter_by_keys`.
//!
//! `verifyDriverCall`'s `Date.parse(String(ts))` is modeled by `date_parse_ms`, a minimal parser
//! for the extended ISO-8601 UTC form `YYYY-MM-DDTHH:MM:SS.sssZ` — exactly the shape every ts
//! value in this twin's own fixtures (and `new Date().toISOString()` output) takes — NOT the full
//! ECMA-262 Date Time String grammar (no bare timezone offsets, no date-only form, no local-time
//! fallback); any ts outside this shape is treated as unparseable, same class as other
//! honestly-disclosed narrow gaps in this crate. `Date.now()` is the frozen-clock literal
//! `1767225600000i64` (`2026-01-01T00:00:00.000Z`), per the crossing playbook's datetime lane.
//!
//! Cross-call fidelity note: `verifyDriverCall` re-derives the expected hmac using the SAME
//! action/input/ts/token/actor it received. JS re-packages them into a fresh
//! `{ action, input, ts, token, actor }` object literal — which, in JS, still carries an
//! `undefined` action as a genuine (if valueless) own property, so `signDriverCall`'s `${action}`
//! template still renders "undefined" exactly as a direct call would. `serde_json::Value` has no
//! `undefined`, so this twin threads the original `Option<&Value>` fields straight through a
//! shared private `sign_driver_call_inner` (used by both the public `signDriverCall` and
//! `verifyDriverCall`) instead of round-tripping through a `Value` object — round-tripping would
//! wrongly turn a MISSING `action` into an explicit JSON `null`, flipping the template output from
//! "undefined" to "null".

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }

const DEFAULT_FRESHNESS_MS: i64 = 5 * 60 * 1000;
// The freshness window is only meaningful against the SAME clock that minted driver_ts. A
// hardcoded instant here meant age_ms = frozen_now - real_ts ≈ -221 days, so every
// driver-sourced dispatch was refused on the Rust engine. crate::clock keeps the two ends
// on one clock: pinned under test/ANTILOKI_FROZEN_CLOCK, wall-clock in production.
fn now_ms() -> i64 { crate::clock::now_ms() }
const FROZEN_NOW_ISO: &str = "2026-01-01T00:00:00.000Z";

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
// AUTH_DIR = join(HOME_DIR, 'auth'); driver-token.mjs: TOKEN_PATH = resolve(AUTH_DIR, 'driver.token').
fn auth_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("auth")
}

fn token_path() -> PathBuf {
    auth_dir().join("driver.token")
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

// ---- hashInput ----------------------------------------------------------------------------

fn js_own_keys(v: &Value) -> Vec<String> {
    match v {
        Value::Object(m) => m.keys().cloned().collect(),
        Value::Array(a) => (0..a.len()).map(|i| i.to_string()).collect(),
        _ => Vec::new(),
    }
}

// JSON.stringify(value, keysArray) — filters object properties (recursively) to those named in
// `keys`; arrays keep every element (only nested objects within them get filtered); primitives
// pass through untouched. `keys` is already sorted by the caller (Object.keys(...).sort()).
fn json_filter_by_keys(v: &Value, keys: &[String]) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = Map::new();
            for k in keys {
                if let Some(val) = m.get(k) {
                    out.insert(k.clone(), json_filter_by_keys(val, keys));
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| json_filter_by_keys(x, keys)).collect()),
        other => other.clone(),
    }
}

fn hash_input_inner(input: Option<&Value>) -> String {
    // `input ?? null` / `input ?? {}` — both undefined and null collapse to the same default.
    let effective: Value = match input { None | Some(Value::Null) => Value::Null, Some(v) => v.clone() };
    let keys_source: Value = match input { None | Some(Value::Null) => Value::Object(Map::new()), Some(v) => v.clone() };
    let mut keys = js_own_keys(&keys_source);
    keys.sort();
    let filtered = json_filter_by_keys(&effective, &keys);
    let canonical = serde_json::to_string(&filtered).unwrap();
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

/// `export function hashInput(input)`
pub fn hashInput(input: &Value) -> Value {
    json!(hash_input_inner(Some(input)))
}

// ---- signDriverCall -------------------------------------------------------------------------

fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
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

// Shared by `signDriverCall` and `verifyDriverCall`'s internal re-derivation — see module doc's
// "cross-call fidelity note" for why this takes `Option<&Value>` instead of a re-wrapped `Value`.
fn sign_driver_call_inner(
    action: Option<&Value>,
    input: Option<&Value>,
    ts: Option<&Value>,
    token: Option<&Value>,
    actor: Option<&Value>,
) -> Value {
    let ts_iso: Value = match nz(ts) { Some(v) => v.clone(), None => json!(FROZEN_NOW_ISO) };
    let input_hash = hash_input_inner(input);
    let actor_str = match nz(actor) { Some(v) => tmpl(Some(v)), None => String::new() };
    let payload = format!("{}:{}:{}:{}", tmpl(action), input_hash, tmpl(Some(&ts_iso)), actor_str);
    let token_bytes = token.and_then(Value::as_str).unwrap_or("");
    let hmac_hex = hmac_sha256_hex(token_bytes.as_bytes(), payload.as_bytes());

    let mut out = Map::new();
    out.insert("hmac".into(), json!(hmac_hex));
    out.insert("ts".into(), ts_iso);
    out.insert("inputHash".into(), json!(input_hash));
    Value::Object(out)
}

/// `export function signDriverCall({ action, input, ts, token, actor })`
pub fn signDriverCall(opts: &Value) -> Value {
    sign_driver_call_inner(opts.get("action"), opts.get("input"), opts.get("ts"), opts.get("token"), opts.get("actor"))
}

// ---- verifyDriverCall -----------------------------------------------------------------------

// Howard Hinnant's days-from-civil (proleptic Gregorian, days since 1970-01-01), verified against
// real `Date.UTC` for the range this twin's fixtures exercise.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn date_parse_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date_part, time_part) = s.split_once('T')?;
    let date_fields: Vec<&str> = date_part.split('-').collect();
    if date_fields.len() != 3 { return None; }
    let year: i64 = date_fields[0].parse().ok()?;
    let month: i64 = date_fields[1].parse().ok()?;
    let day: i64 = date_fields[2].parse().ok()?;

    let (hms_part, ms_part) = match time_part.split_once('.') {
        Some((h, m)) => (h, m),
        None => (time_part, "0"),
    };
    let time_fields: Vec<&str> = hms_part.split(':').collect();
    if time_fields.len() != 3 { return None; }
    let hour: i64 = time_fields[0].parse().ok()?;
    let minute: i64 = time_fields[1].parse().ok()?;
    let second: i64 = time_fields[2].parse().ok()?;
    let ms_padded = format!("{ms_part:0<3}");
    let millis: i64 = ms_padded.get(0..3)?.parse().ok()?;

    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=59).contains(&second)
    {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    Some(secs * 1000 + millis)
}

/// `export function verifyDriverCall({ action, input, ts, hmac, token, actor,
/// freshnessMs = DEFAULT_FRESHNESS_MS })`
pub fn verifyDriverCall(opts: &Value) -> Value {
    let action = opts.get("action");
    let input = opts.get("input");
    let ts = opts.get("ts");
    let hmac_in = opts.get("hmac");
    let token = opts.get("token");
    let actor = opts.get("actor");

    if !truthy(token) {
        return json!({ "ok": false, "reason": "no driver token loaded on this host" });
    }
    if !truthy(hmac_in) {
        return json!({ "ok": false, "reason": "context.driver_hmac missing" });
    }
    if !truthy(ts) {
        return json!({ "ok": false, "reason": "context.driver_ts missing" });
    }

    let parsed = date_parse_ms(&js_string(ts.unwrap()));
    let t = match parsed {
        Some(v) => v,
        None => {
            return json!({
                "ok": false,
                "reason": format!("driver_ts '{}' is not valid ISO 8601", tmpl(ts)),
            });
        }
    };

    let freshness_val: Value = match opts.get("freshnessMs") {
        None => json!(DEFAULT_FRESHNESS_MS),
        Some(v) => v.clone(),
    };
    let freshness_num = js_number(Some(&freshness_val));
    let age = now_ms() - t;
    if (age as f64) > freshness_num || (age as f64) < -10_000.0 {
        return json!({
            "ok": false,
            "reason": format!("driver call too old: age_ms={}, freshness_ms={}", age, tmpl(Some(&freshness_val))),
        });
    }

    let signed = sign_driver_call_inner(action, input, ts, token, actor);
    let expected = signed.get("hmac").and_then(Value::as_str).unwrap_or("").to_string();
    let hmac_str = hmac_in.and_then(Value::as_str).unwrap_or("").to_string();

    if expected.len() != hmac_str.len() {
        return json!({ "ok": false, "reason": "hmac length mismatch" });
    }
    // Constant-time compare, same as JS.
    let mut diff: u32 = 0;
    for (a, b) in expected.bytes().zip(hmac_str.bytes()) {
        diff |= (a ^ b) as u32;
    }
    if diff != 0 {
        return json!({ "ok": false, "reason": "hmac mismatch — wrong token or tampered payload" });
    }

    json!({ "ok": true })
}

// ---- bespoke-native fs units (see module doc) ------------------------------------------------

fn write_token_file(path: &Path, token: &str) -> Result<(), String> {
    std::fs::write(path, token).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // JS: `try { chmodSync(TOKEN_PATH, 0o600); } catch { /* Windows: best effort */ }`
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// `export function loadOrCreateDriverToken()`
pub fn loadOrCreateDriverToken() -> Result<String, String> {
    let path = token_path();
    if path.exists() {
        let s = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        return Ok(s.trim().to_string());
    }
    let key = os_random_bytes(32)?;
    let token = hex::encode(key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    write_token_file(&path, &token)?;
    Ok(token)
}

/// `export function readDriverToken()`
pub fn readDriverToken() -> Value {
    let path = token_path();
    if !path.exists() {
        return Value::Null;
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => json!(s.trim()),
        Err(_) => Value::Null,
    }
}

/// `export default function driverToken(_input, _context)` — `_context` is unused (JS ignores
/// both params), mirrored by the unused `_db` param below.
pub fn driverToken(_input: &Value, _db: &Connection) -> Result<Value, String> {
    loadOrCreateDriverToken().map(|s| json!(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default, and this same var is read by other converted twins' tests too —
    // shared crate-wide lock (test_env_lock.rs), same convention as god-driver-halt.rs /
    // tui-god-token.rs / auth-init.rs.
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
        dir.push(format!("antiloki-driver-token-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn load_or_create_generates_a_64_hex_char_token_and_persists_it() {
        let dir = temp_dir("create");
        let token = with_home(dir.to_str().unwrap(), || loadOrCreateDriverToken().unwrap());
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let path = dir.join("auth").join("driver.token");
        assert!(path.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), token);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_create_is_idempotent_second_call_returns_same_token() {
        let dir = temp_dir("idempotent");
        let first = with_home(dir.to_str().unwrap(), || loadOrCreateDriverToken().unwrap());
        let second = with_home(dir.to_str().unwrap(), || loadOrCreateDriverToken().unwrap());
        assert_eq!(first, second);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_create_trims_whitespace_from_an_existing_file() {
        let dir = temp_dir("trim");
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(auth_dir.join("driver.token"), "  the-existing-token\n").unwrap();
        let token = with_home(dir.to_str().unwrap(), || loadOrCreateDriverToken().unwrap());
        assert_eq!(token, "the-existing-token");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn load_or_create_writes_the_file_chmod_600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("chmod");
        with_home(dir.to_str().unwrap(), || loadOrCreateDriverToken().unwrap());
        let path = dir.join("auth").join("driver.token");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_driver_token_null_when_missing() {
        let dir = temp_dir("read-missing");
        let r = with_home(dir.to_str().unwrap(), readDriverToken);
        assert_eq!(r, Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_driver_token_returns_trimmed_contents_when_present() {
        let dir = temp_dir("read-present");
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(auth_dir.join("driver.token"), "  a-token-value \n").unwrap();
        let r = with_home(dir.to_str().unwrap(), readDriverToken);
        assert_eq!(r, json!("a-token-value"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn driver_token_default_export_creates_and_returns_token() {
        let dir = temp_dir("default-export");
        let r = with_home(dir.to_str().unwrap(), || driverToken(&json!({}), &db0()).unwrap());
        let s = r.as_str().unwrap();
        assert_eq!(s.len(), 64);
        let via_read = with_home(dir.to_str().unwrap(), readDriverToken);
        assert_eq!(via_read, json!(s));
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- pure functions: cross-checked against real Node (skills/audit/driver-token.mjs) ----

    #[test]
    fn hash_input_null_matches_real_js_vector() {
        assert_eq!(
            hashInput(&Value::Null),
            json!("74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b")
        );
    }

    #[test]
    fn hash_input_flat_object_sorts_keys_matches_real_js_vector() {
        assert_eq!(
            hashInput(&json!({"zebra":"z","apple":"a","mango":"m"})),
            json!("9e9353d5906629ecb81dc632365d0a2daaf240b8282f3a747b63077c85963b45")
        );
    }

    #[test]
    fn hash_input_nested_filters_recursively_by_same_key_set_matches_real_js_vector() {
        assert_eq!(
            hashInput(&json!({"b":1,"a":{"x":2,"y":3,"b":99},"c":[1,2,{"d":1,"a":2}]})),
            json!("bd469d44b61d58d8a599fe5cd0223f725036525f790274a72dfd7cefe69e0e91")
        );
    }

    #[test]
    fn hash_input_array_top_level_matches_real_js_vector() {
        assert_eq!(
            hashInput(&json!([1, 2, 3])),
            json!("a615eeaee21de5179de080de8c3052c8da901138406ba71c38c032845f7d54f4")
        );
    }

    #[test]
    fn hash_input_empty_object_matches_real_js_vector() {
        assert_eq!(hashInput(&json!({})), json!("44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"));
    }

    #[test]
    fn hash_input_string_matches_real_js_vector() {
        assert_eq!(hashInput(&json!("hello")), json!("5aa762ae383fbb727af3c7a36d4940a5b8c40a989452d2304fc958ff3f354e7a"));
    }

    #[test]
    fn sign_driver_call_matches_real_js_vector() {
        let r = signDriverCall(&json!({
            "action": "card.update",
            "input": {"cardId": "CARD-1"},
            "ts": "2026-01-01T00:00:00.000Z",
            "token": "super-secret-token-hex",
            "actor": "driver:bin",
        }));
        assert_eq!(
            r,
            json!({
                "hmac": "4bd18a2b591208fdd1fdf5c31cba27f6bec332316dfb7ddfbf1d2a804198af09",
                "ts": "2026-01-01T00:00:00.000Z",
                "inputHash": "078ddf4e2bd380d3e74c38ae15fdadccc8693582b23955e4666fd3a629559280",
            })
        );
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["hmac", "ts", "inputHash"]);
    }

    #[test]
    fn sign_driver_call_omitted_ts_uses_frozen_now() {
        let r = signDriverCall(&json!({ "action": "x", "input": null, "token": "t", "actor": "a" }));
        assert_eq!(r["ts"], json!(FROZEN_NOW_ISO));
    }

    #[test]
    fn sign_driver_call_omitted_action_renders_undefined_in_payload_distinct_from_null() {
        let with_missing = signDriverCall(&json!({ "input": null, "ts": "2026-01-01T00:00:00.000Z", "token": "t" }));
        let with_null = signDriverCall(&json!({ "action": null, "input": null, "ts": "2026-01-01T00:00:00.000Z", "token": "t" }));
        assert_ne!(with_missing["hmac"], with_null["hmac"]);
    }

    #[test]
    fn verify_driver_call_ok_true_round_trips_with_sign() {
        let opts = json!({
            "action": "card.update", "input": {"cardId": "CARD-1"},
            "ts": "2026-01-01T00:00:00.000Z", "token": "super-secret-token-hex", "actor": "driver:bin",
        });
        let signed = signDriverCall(&opts);
        let mut verify_opts = opts.as_object().unwrap().clone();
        verify_opts.insert("hmac".into(), signed["hmac"].clone());
        let r = verifyDriverCall(&Value::Object(verify_opts));
        assert_eq!(r, json!({ "ok": true }));
    }

    #[test]
    fn verify_driver_call_too_old_beyond_default_freshness() {
        let opts = json!({
            "action": "a", "input": null, "ts": "2025-12-31T23:54:00.000Z",
            "token": "t", "actor": "x",
        });
        let signed = signDriverCall(&opts);
        let mut verify_opts = opts.as_object().unwrap().clone();
        verify_opts.insert("hmac".into(), signed["hmac"].clone());
        let r = verifyDriverCall(&Value::Object(verify_opts));
        assert_eq!(r, json!({ "ok": false, "reason": "driver call too old: age_ms=360000, freshness_ms=300000" }));
    }

    #[test]
    fn verify_driver_call_too_far_in_the_future() {
        let opts = json!({
            "action": "a", "input": null, "ts": "2026-01-01T00:00:15.000Z",
            "token": "t", "actor": "x",
        });
        let signed = signDriverCall(&opts);
        let mut verify_opts = opts.as_object().unwrap().clone();
        verify_opts.insert("hmac".into(), signed["hmac"].clone());
        let r = verifyDriverCall(&Value::Object(verify_opts));
        assert_eq!(r, json!({ "ok": false, "reason": "driver call too old: age_ms=-15000, freshness_ms=300000" }));
    }

    #[test]
    fn verify_driver_call_invalid_ts_not_iso8601() {
        let r = verifyDriverCall(&json!({ "action": "a", "ts": "not-a-date", "hmac": "deadbeef", "token": "t" }));
        assert_eq!(r, json!({ "ok": false, "reason": "driver_ts 'not-a-date' is not valid ISO 8601" }));
    }

    #[test]
    fn verify_driver_call_missing_token_hmac_ts() {
        assert_eq!(
            verifyDriverCall(&json!({ "ts": "2026-01-01T00:00:00.000Z", "hmac": "x" })),
            json!({ "ok": false, "reason": "no driver token loaded on this host" })
        );
        assert_eq!(
            verifyDriverCall(&json!({ "ts": "2026-01-01T00:00:00.000Z", "token": "t" })),
            json!({ "ok": false, "reason": "context.driver_hmac missing" })
        );
        assert_eq!(
            verifyDriverCall(&json!({ "token": "t", "hmac": "x" })),
            json!({ "ok": false, "reason": "context.driver_ts missing" })
        );
    }

    #[test]
    fn verify_driver_call_hmac_length_mismatch() {
        let r = verifyDriverCall(&json!({
            "action": "a", "ts": "2026-01-01T00:00:00.000Z", "hmac": "short", "token": "t",
        }));
        assert_eq!(r, json!({ "ok": false, "reason": "hmac length mismatch" }));
    }

    #[test]
    fn verify_driver_call_hmac_mismatch_same_length() {
        let opts = json!({ "action": "a", "input": null, "ts": "2026-01-01T00:00:00.000Z", "token": "t", "actor": "x" });
        let signed = signDriverCall(&opts);
        let real_hmac = signed["hmac"].as_str().unwrap();
        let mut flipped: Vec<u8> = real_hmac.bytes().collect();
        let last = *flipped.last().unwrap();
        *flipped.last_mut().unwrap() = if last == b'0' { b'1' } else { b'0' };
        let wrong_hmac = String::from_utf8(flipped).unwrap();
        let mut verify_opts = opts.as_object().unwrap().clone();
        verify_opts.insert("hmac".into(), json!(wrong_hmac));
        let r = verifyDriverCall(&Value::Object(verify_opts));
        assert_eq!(r, json!({ "ok": false, "reason": "hmac mismatch — wrong token or tampered payload" }));
    }

    #[test]
    fn verify_driver_call_custom_freshness_ms_narrows_the_window() {
        let opts = json!({ "action": "a", "input": null, "ts": "2025-12-31T23:59:00.000Z", "token": "t", "actor": "x" });
        let signed = signDriverCall(&opts);
        let mut too_tight = opts.as_object().unwrap().clone();
        too_tight.insert("hmac".into(), signed["hmac"].clone());
        too_tight.insert("freshnessMs".into(), json!(30000));
        assert_eq!(
            verifyDriverCall(&Value::Object(too_tight)),
            json!({ "ok": false, "reason": "driver call too old: age_ms=60000, freshness_ms=30000" })
        );

        let mut wide_enough = opts.as_object().unwrap().clone();
        wide_enough.insert("hmac".into(), signed["hmac"].clone());
        wide_enough.insert("freshnessMs".into(), json!(120000));
        assert_eq!(verifyDriverCall(&Value::Object(wide_enough)), json!({ "ok": true }));
    }

    #[test]
    fn date_parse_ms_rejects_garbage() {
        assert_eq!(date_parse_ms("not-a-date"), None);
    }

    #[test]
    fn date_parse_ms_matches_frozen_now() {
        assert_eq!(date_parse_ms(FROZEN_NOW_ISO), Some(crate::clock::FROZEN_MS));
    }
}
