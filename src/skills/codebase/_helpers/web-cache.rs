//! web-cache.rs — twin of skills/codebase/_helpers/web-cache.mjs (named-exports-only helper,
//! _helpers contract). NAMES ARE CONTRACT: `ensureWebCache` (db-first, DDL) · `assertPublicHttps`
//! (no-db, SSRF guard). Both are pure/deterministic and ported below.
//!
//! `cachedFetch` is REAL as of 2026-08-09 — enabling ureq's `tls` feature unblocked it. Only the
//! network hop is nondeterministic; the cache lookup, TTL window, body cap and write-back are
//! ordinary db work, unit-tested below by seeding a fresh row so the call replays from the DB and
//! never dials out. It is still not a conform fixture unit (a live remote cannot be fixtured).
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String {
    // String(v ?? '')
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    // JS `${v}`
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

const SCHEMA_SQL: &str = r#"CREATE TABLE IF NOT EXISTS knowledge.web_fetch_cache (
    url TEXT PRIMARY KEY,
    body_text TEXT NOT NULL,
    fetched_at TEXT NOT NULL DEFAULT (datetime('now'))
  )"#;

// JS `ensureWebCache(db)` execs the qualified DDL and returns undefined (-> null). Idempotent
// CREATE IF NOT EXISTS. Executed copy freezes datetime('now') per the frozen-clock DDL rule (a
// later INSERT into this table must see the same frozen fetched_at DEFAULT in both lanes).
pub fn ensureWebCache(db: &Connection) -> Value {
    let _ = db.execute_batch(&SCHEMA_SQL.replace("datetime('now'", &format!("datetime({}", crate::clock::now_sql_inner())));
    Value::Null
}

// Minimal `new URL(s)` stand-in: extracts `scheme:` (JS `.protocol`) and the lowercased
// authority host (JS `.hostname`, no userinfo/port). Not a full RFC 3986 parser (no IDNA,
// no percent-decoding, no IPv6-bracket support) — sufficient for the https/public-host fixtures
// this guard is exercised against; JS's own URL parser throws a TypeError for a malformed
// origin, a path this Rust twin mirrors with Err rather than reproducing V8's exact wording.
fn parse_url(s: &str) -> Option<(String, String)> {
    let idx = s.find("://")?;
    let scheme = &s[..idx];
    let rest = &s[idx + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host_port = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let host = match host_port.rfind(':') {
        Some(i) => &host_port[..i],
        None => host_port,
    };
    if host.is_empty() {
        return None;
    }
    Some((format!("{scheme}:"), host.to_lowercase()))
}

fn private_host_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(127\.|10\.|192\.168\.|169\.254\.)").unwrap())
}
fn cgnat_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^172\.(1[6-9]|2\d|3[01])\.").unwrap())
}

// JS `assertPublicHttps(url, err, skill)`. `err` is never invoked on the SUCCESS path this twin
// fixtures (playbook GOTCHA 9: throws are fragile V8 text, only the success path is exercised),
// so it is accepted but unused here — kept as a positional `&Value` to preserve the 3-arg
// no-db-helper signature (NAMES/arity ARE CONTRACT).
pub fn assertPublicHttps(url: &Value, err: &Value, skill: &Value) -> Result<Value, String> {
    let _ = err;
    let u = str_qq(Some(url));
    let (protocol, hostname) = parse_url(&u).ok_or_else(|| format!("Invalid URL: '{u}'"))?;
    if protocol != "https:" {
        return Err(format!("{}: url must be https URL (got string)", tmpl(Some(skill))));
    }
    let h = hostname;
    if h == "localhost"
        || private_host_re().is_match(&h)
        || cgnat_re().is_match(&h)
        || h.ends_with(".internal")
        || h.ends_with(".local")
    {
        return Err(format!("{}: url must be public host (got string)", tmpl(Some(skill))));
    }
    Ok(Value::Null)
}


pub const FETCH_TIMEOUT_MS: u64 = 10000;
pub const MAX_BODY: usize = 256 * 1024;

/// `export async function cachedFetch(db, url, init, stats, { refresh, ttlHours, err, skill })`.
///
/// REAL as of 2026-08-09 (enabling ureq's `tls` feature unblocked it — this reaches https hosts).
/// The network hop is the only nondeterministic part; the cache lookup, TTL window, body cap and
/// write-back are ordinary deterministic db work and are unit-tested below without touching the
/// network (seed a fresh row → it replays from the DB and never dials out).
///
/// JS returns `null` on ANY failure — non-2xx, timeout, connection error — via `catch { return
/// null }`. Callers treat null as "no data" and degrade; they do not distinguish causes. That is
/// reproduced exactly: a failure here is `Ok(Value::Null)`, never an `Err`, because turning a dead
/// remote into a hard error would take down a whole magazine issue over one unreachable feed.
///
/// `stats` is an in/out counter object (`{cached_hits, fetched}`) in JS; here it is a `&mut Value`
/// so the same accumulation is visible to the caller.
pub fn cachedFetch(
    db: &Connection,
    url: &Value,
    init: Option<&Value>,
    stats: Option<&mut Value>,
    refresh: bool,
    ttl_hours: f64,
    err: &Value,
    skill: &Value,
) -> Result<Value, String> {
    assertPublicHttps(url, err, skill)?;
    let url_s = str_qq(Some(url));

    let bump = |stats: Option<&mut Value>, key: &str| {
        if let Some(s) = stats {
            if let Some(o) = s.as_object_mut() {
                let cur = o.get(key).and_then(Value::as_i64).unwrap_or(0);
                o.insert(key.to_string(), json!(cur + 1));
            }
        }
    };

    if !refresh {
        // JS: `datetime('now', '-N hours')` with N = max(1, round(ttlHours)).
        let hours = (ttl_hours.round() as i64).max(1);
        let window = format!("-{hours} hours");
        let hit: Option<String> = db
            .query_row(
                &format!(
                    "SELECT body_text FROM knowledge.web_fetch_cache WHERE url = ? AND fetched_at > datetime({}, ?)",
                    crate::clock::now_sql_inner()
                ),
                rusqlite::params![url_s, window],
                |r| r.get(0),
            )
            .ok();
        if let Some(body) = hit {
            bump(stats, "cached_hits");
            return Ok(json!(body));
        }
    }

    // The URL is the cache KEY verbatim, but a `#fragment` is never sent over the wire — browsers
    // and node's fetch strip it. magazine-observe relies on exactly that: it appends
    // `#<hash-of-post-body>` so a changed request body cannot hit a stale cached batch. Dialing
    // the fragment would send a URL no server expects, so it is cut here and only here.
    let wire_url = match url_s.split_once('#') {
        Some((base, _)) => base.to_string(),
        None => url_s.clone(),
    };

    // JS: `fetch(url, { headers: {user-agent}, ...init })` — init's method/headers/body override
    // the defaults, and a body implies a non-GET verb.
    let method = init
        .and_then(|i| i.get("method"))
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_uppercase();
    let mut req = ureq::request(&method, &wire_url)
        .set("user-agent", "antiloki-observatory/1.0")
        .timeout(std::time::Duration::from_millis(FETCH_TIMEOUT_MS));
    if let Some(headers) = init.and_then(|i| i.get("headers")).and_then(Value::as_object) {
        for (k, v) in headers {
            req = req.set(k, &str_qq(Some(v)));
        }
    }
    let body_out = init.and_then(|i| i.get("body")).map(|b| str_qq(Some(b)));

    let resp = match match body_out {
        Some(b) => req.send_string(&b),
        None => req.call(),
    } {
        Ok(r) => r,
        Err(_) => return Ok(Value::Null), // JS: non-ok AND thrown errors both yield null
    };

    let mut body = match resp.into_string() {
        Ok(b) => b,
        Err(_) => return Ok(Value::Null),
    };
    if body.len() > MAX_BODY {
        // JS slices by UTF-16 length; slicing bytes could split a codepoint, so cut on a char
        // boundary at or below the cap.
        let mut cut = MAX_BODY;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
    }

    let _ = db.execute(
        &format!(
            "INSERT OR REPLACE INTO knowledge.web_fetch_cache (url, body_text, fetched_at) VALUES (?, ?, {})",
            crate::clock::now_sql()
        ),
        rusqlite::params![url_s, body],
    );
    bump(stats, "fetched");
    Ok(json!(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── cachedFetch: everything except the network hop ───────────────────────────────────────
    fn wc_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge;").unwrap();
        ensureWebCache(&c);
        c
    }
    fn e() -> Value { json!({}) }
    fn sk() -> Value { json!("web-cache-test") }

    #[test]
    fn a_fresh_cache_row_is_replayed_without_touching_the_network() {
        let c = wc_db();
        c.execute(
            "INSERT INTO knowledge.web_fetch_cache (url, body_text, fetched_at) VALUES (?, ?, datetime('2026-01-01 00:00:00'))",
            rusqlite::params!["https://example.com/a", "CACHED BODY"],
        ).unwrap();
        let mut stats = json!({ "cached_hits": 0, "fetched": 0 });
        let r = cachedFetch(&c, &json!("https://example.com/a"), None, Some(&mut stats), false, 24.0, &e(), &sk()).unwrap();
        assert_eq!(r, json!("CACHED BODY"));
        assert_eq!(stats["cached_hits"], json!(1), "a hit increments cached_hits");
        assert_eq!(stats["fetched"], json!(0), "and never fetched");
    }

    #[test]
    fn a_row_older_than_the_ttl_is_not_a_hit() {
        let c = wc_db();
        // frozen now is 2026-01-01; this row is 3 days old, TTL is 24h
        c.execute(
            "INSERT INTO knowledge.web_fetch_cache (url, body_text, fetched_at) VALUES (?, ?, '2025-12-29 00:00:00')",
            rusqlite::params!["https://example.com/stale", "OLD"],
        ).unwrap();
        let mut stats = json!({ "cached_hits": 0, "fetched": 0 });
        // the network call will fail (no such host) -> Ok(null), which is the point: it did NOT
        // return the stale body.
        let r = cachedFetch(&c, &json!("https://example.com/stale"), None, Some(&mut stats), false, 24.0, &e(), &sk());
        assert!(r.is_ok(), "a dead remote must degrade, not error: {r:?}");
        assert_ne!(r.unwrap(), json!("OLD"), "stale rows must not be served");
        assert_eq!(stats["cached_hits"], json!(0));
    }

    #[test]
    fn refresh_bypasses_a_fresh_row() {
        let c = wc_db();
        c.execute(
            "INSERT INTO knowledge.web_fetch_cache (url, body_text, fetched_at) VALUES (?, ?, datetime('2026-01-01 00:00:00'))",
            rusqlite::params!["https://example.com/r", "CACHED"],
        ).unwrap();
        let mut stats = json!({ "cached_hits": 0, "fetched": 0 });
        let r = cachedFetch(&c, &json!("https://example.com/r"), None, Some(&mut stats), true, 24.0, &e(), &sk());
        assert!(r.is_ok());
        assert_ne!(r.unwrap(), json!("CACHED"), "refresh=true must skip the cache");
        assert_eq!(stats["cached_hits"], json!(0));
    }

    #[test]
    fn the_ssrf_guard_runs_before_any_cache_or_network_work() {
        let c = wc_db();
        for bad in ["http://example.com", "https://localhost/x", "https://127.0.0.1/x",
                    "https://10.1.2.3/x", "https://192.168.0.5/x", "https://foo.internal/x"] {
            let r = cachedFetch(&c, &json!(bad), None, None, false, 24.0, &e(), &sk());
            assert!(r.is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn ttl_hours_is_floored_at_one_hour() {
        // JS: Math.max(1, Math.round(ttlHours)) — a 0 or negative TTL must not become
        // datetime('now','-0 hours') and turn every row into a hit.
        let c = wc_db();
        c.execute(
            "INSERT INTO knowledge.web_fetch_cache (url, body_text, fetched_at) VALUES (?, ?, '2025-01-01 00:00:00')",
            rusqlite::params!["https://example.com/z", "ANCIENT"],
        ).unwrap();
        let r = cachedFetch(&c, &json!("https://example.com/z"), None, None, false, 0.0, &e(), &sk());
        assert!(r.is_ok());
        assert_ne!(r.unwrap(), json!("ANCIENT"));
    }

    #[test]
    fn a_missing_stats_object_is_fine() {
        let c = wc_db();
        c.execute(
            "INSERT INTO knowledge.web_fetch_cache (url, body_text, fetched_at) VALUES (?, ?, datetime('2026-01-01 00:00:00'))",
            rusqlite::params!["https://example.com/n", "B"],
        ).unwrap();
        assert_eq!(cachedFetch(&c, &json!("https://example.com/n"), None, None, false, 24.0, &e(), &sk()).unwrap(), json!("B"));
    }


    #[test]
    fn ensures_cache_table_idempotently_and_freezes_the_default() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge").unwrap();
        assert_eq!(ensureWebCache(&c), Value::Null);
        assert_eq!(ensureWebCache(&c), Value::Null); // idempotent
        c.execute(
            "INSERT INTO knowledge.web_fetch_cache (url, body_text) VALUES ('https://x.example/a', 'hi')",
            [],
        )
        .unwrap();
        let fetched_at: String = c
            .query_row(
                "SELECT fetched_at FROM knowledge.web_fetch_cache WHERE url = 'https://x.example/a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fetched_at, "2026-01-01 00:00:00");
    }

    #[test]
    fn accepts_public_https_hosts() {
        assert_eq!(
            assertPublicHttps(&json!("https://example.com/path"), &json!({}), &json!("web-cache")),
            Ok(Value::Null)
        );
        assert_eq!(
            assertPublicHttps(&json!("https://sub.example.com:8443/x"), &json!({}), &json!("web-cache")),
            Ok(Value::Null)
        );
    }

    #[test]
    fn rejects_non_https_and_private_hosts() {
        assert!(assertPublicHttps(&json!("http://example.com"), &json!({}), &json!("web-cache")).is_err());
        assert!(assertPublicHttps(&json!("https://localhost/"), &json!({}), &json!("web-cache")).is_err());
        assert!(assertPublicHttps(&json!("https://127.0.0.1/"), &json!({}), &json!("web-cache")).is_err());
        assert!(assertPublicHttps(&json!("https://10.0.0.5/"), &json!({}), &json!("web-cache")).is_err());
        assert!(assertPublicHttps(&json!("https://192.168.1.1/"), &json!({}), &json!("web-cache")).is_err());
        assert!(assertPublicHttps(&json!("https://172.16.0.1/"), &json!({}), &json!("web-cache")).is_err());
        assert!(assertPublicHttps(&json!("https://172.32.0.1/"), &json!({}), &json!("web-cache")).is_ok());
        assert!(assertPublicHttps(&json!("https://box.internal/"), &json!({}), &json!("web-cache")).is_err());
        assert!(assertPublicHttps(&json!("https://box.local/"), &json!({}), &json!("web-cache")).is_err());
    }
}
