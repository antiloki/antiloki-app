//! SIGN IN TO AN MCP SERVER BY ITS OWN OAUTH (operator 2026-11-20: "login via mcp so using our system"): the MCP authorization flow, done by the serve. No key to paste, no window a program drives.
//!
//!   1. discover   ask the server (a 401 names where its metadata is), read who may sign the person in and where
//!   2. register   introduce antiloki to that sign-in server as a public client (RFC 7591), once per redirect address
//!   3. begin      a PKCE challenge (RFC 7636, S256) and a one-use state; the person's own browser opens the sign-in page
//!   4. callback   the browser comes back to this machine with a code; the serve trades it for tokens, kept in the private token file
//!   5. refresh    an access token near its end is traded for a new one before a call
//! The person's password is never seen: the sign-in is the server's page in their normal browser. Pure helpers are tested; the rest talks to the server.
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
/// An access token this close to its end is renewed before use.
const RENEW_MS: i64 = 90_000;
const PENDING_MS: i64 = 10 * 60_000;

/// Random URL-safe text of `n` bytes. Not for anything but state and verifiers.
pub fn random_text(n: usize) -> String { use rand::RngCore; let mut b = vec![0u8; n]; rand::thread_rng().fill_bytes(&mut b); B64.encode(b) }
/// The S256 challenge of a verifier (RFC 7636 §4.2). Pure.
pub fn challenge_of(verifier: &str) -> String { B64.encode(Sha256::digest(verifier.as_bytes())) }
fn enc(s: &str) -> String { s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect() }
/// The `resource_metadata` address a 401 names in its `WWW-Authenticate` header, when it does. Pure.
pub fn resource_metadata_of(www_authenticate: &str) -> Option<String> {
    let i = www_authenticate.find("resource_metadata=")? + "resource_metadata=".len(); let rest = &www_authenticate[i..];
    let v = if let Some(r) = rest.strip_prefix('"') { r.split('"').next()? } else { rest.split([',', ' ']).next()? }; if v.starts_with("https://") || v.starts_with("http://127.0.0.1") || v.starts_with("http://localhost") { Some(v.to_string()) } else { None }
}
/// The address of the sign-in page for a person: the server's own, with our client, redirect, challenge and state. Pure.
pub fn authorize_url(endpoint: &str, client_id: &str, redirect: &str, challenge: &str, state: &str, scope: &str, resource: &str) -> String {
    let sep = if endpoint.contains('?') { '&' } else { '?' };
    let mut u = format!("{endpoint}{sep}response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&state={}", enc(client_id), enc(redirect), enc(challenge), enc(state));
    if !scope.is_empty() { u.push_str(&format!("&scope={}", enc(scope))); } if !resource.is_empty() { u.push_str(&format!("&resource={}", enc(resource))); } u
}
/// The tokens in a token-endpoint reply, with the moment the access token ends (ms). Pure.
pub fn tokens_of(reply: &Value, now_ms: i64) -> Result<(String, Option<String>, i64), String> {
    if let Some(e) = reply["error"].as_str() { return Err(format!("the sign-in server said {e}{}", reply["error_description"].as_str().map(|d| format!(": {d}")).unwrap_or_default())); }
    let access = reply["access_token"].as_str().filter(|t| !t.is_empty()).ok_or("the sign-in server gave no access token")?.to_string();
    let exp = now_ms + reply["expires_in"].as_i64().unwrap_or(3600) * 1000; Ok((access, reply["refresh_token"].as_str().filter(|t| !t.is_empty()).map(String::from), exp))
}
/// Does an access token need renewing now? Pure.
pub fn renew_due(expires_at_ms: i64, now_ms: i64) -> bool { expires_at_ms > 0 && expires_at_ms - now_ms < RENEW_MS }

/// What the serve found out about a server's sign-in.
#[derive(Clone, Debug, Default)]
pub struct AuthServer { pub authorize: String, pub token: String, pub register: String, pub resource: String, pub scope: String }
fn http() -> Result<reqwest::Client, String> { reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).user_agent("antiloki").build().map_err(|e| e.to_string()) }
async fn get_json(c: &reqwest::Client, url: &str) -> Result<Value, String> { let r = c.get(url).header("accept", "application/json").send().await.map_err(|e| format!("{url}: {e}"))?; if !r.status().is_success() { return Err(format!("{url} answered {}", r.status())); } r.json::<Value>().await.map_err(|e| format!("{url}: {e}")) }
fn origin_of(url: &str) -> String { let i = url.find("://").map(|i| i + 3).unwrap_or(0); let end = url[i..].find('/').map(|j| i + j).unwrap_or(url.len()); url[..end].to_string() }

/// Ask the MCP server how it signs people in. Blocks nothing.
pub async fn discover(mcp_url: &str) -> Result<AuthServer, String> {
    let c = http()?;
    let probe = c.post(mcp_url).header("content-type", "application/json").header("accept", "application/json, text/event-stream").body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"antiloki","version":"1"}}}"#).send().await.map_err(|e| format!("{mcp_url}: {e}"))?;
    let named = probe.headers().get("www-authenticate").and_then(|h| h.to_str().ok()).and_then(resource_metadata_of);
    let origin = origin_of(mcp_url);
    let prm = match named { Some(u) => get_json(&c, &u).await?, None => get_json(&c, &format!("{origin}/.well-known/oauth-protected-resource")).await? };
    let issuer = prm["authorization_servers"][0].as_str().map(String::from).unwrap_or(origin.clone());
    let meta = match get_json(&c, &format!("{}/.well-known/oauth-authorization-server", issuer.trim_end_matches('/'))).await { Ok(m) => m, Err(_) => get_json(&c, &format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/'))).await? };
    let s = |k: &str| meta[k].as_str().unwrap_or("").to_string();
    if s("authorization_endpoint").is_empty() || s("token_endpoint").is_empty() { return Err("that server publishes no sign-in endpoints".into()); }
    if !meta["code_challenge_methods_supported"].as_array().map_or(true, |a| a.iter().any(|m| m == "S256")) { return Err("that server does not take S256 sign-in challenges".into()); }
    let scopes: Vec<String> = prm["scopes_supported"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    Ok(AuthServer { authorize: s("authorization_endpoint"), token: s("token_endpoint"), register: s("registration_endpoint"), resource: prm["resource"].as_str().map(String::from).unwrap_or(mcp_url.to_string()), scope: scopes.join(" ") })
}
/// Introduce antiloki to the sign-in server (dynamic client registration, a public client). Returns its client id.
pub async fn register(server: &AuthServer, redirect: &str) -> Result<String, String> {
    if server.register.is_empty() { return Err("that server does not let apps register themselves: paste a key instead".into()); }
    let r = http()?.post(&server.register).json(&json!({ "client_name": "antiloki", "redirect_uris": [redirect], "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"], "token_endpoint_auth_method": "none" })).send().await.map_err(|e| format!("registration: {e}"))?;
    let st = r.status(); let v: Value = r.json().await.unwrap_or(Value::Null);
    if !st.is_success() { return Err(format!("registration answered {st}: {}", v["error_description"].as_str().or(v["error"].as_str()).unwrap_or(""))); }
    v["client_id"].as_str().map(String::from).ok_or_else(|| "registration gave no client id".into())
}

struct Pending { app: String, verifier: String, redirect: String, client_id: String, server: AuthServer, at: i64 }
fn pending() -> &'static Mutex<HashMap<String, Pending>> { static P: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new(); P.get_or_init(|| Mutex::new(HashMap::new())) }
fn now_ms() -> i64 { crate::cli::now_ms() as i64 }
/// The address the sign-in server sends the browser back to: this serve, on this machine.
pub fn redirect_uri() -> String { let port = std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41998); format!("http://127.0.0.1:{port}/api/v1/webapps/oauth/callback") }

/// Start a sign-in: the page to open in the person's browser.
pub async fn begin(app: &str, mcp_url: &str) -> Result<String, String> {
    let server = discover(mcp_url).await?; let redirect = redirect_uri();
    let client_id = register(&server, &redirect).await?;
    let (verifier, state) = (random_text(48), random_text(24));
    let url = authorize_url(&server.authorize, &client_id, &redirect, &challenge_of(&verifier), &state, &server.scope, &server.resource);
    let mut p = pending().lock().unwrap_or_else(|e| e.into_inner()); let t = now_ms(); p.retain(|_, v| t - v.at < PENDING_MS);
    p.insert(state, Pending { app: app.to_string(), verifier, redirect, client_id, server, at: t }); Ok(url)
}
/// The browser came back with a code: trade it for tokens and keep them. Returns the app that is now signed in.
pub async fn finish(state: &str, code: &str) -> Result<String, String> {
    let p = { let mut m = pending().lock().unwrap_or_else(|e| e.into_inner()); m.remove(state) }.ok_or("that sign-in is not one this machine started (or it took too long): start it again")?;
    if now_ms() - p.at > PENDING_MS { return Err("that sign-in took too long: start it again".into()); }
    let r = http()?.post(&p.server.token).header("accept", "application/json").form(&[("grant_type", "authorization_code"), ("code", code), ("redirect_uri", p.redirect.as_str()), ("client_id", p.client_id.as_str()), ("code_verifier", p.verifier.as_str()), ("resource", p.server.resource.as_str())]).send().await.map_err(|e| format!("token: {e}"))?;
    let v: Value = r.json().await.map_err(|e| format!("token: {e}"))?; let (access, refresh, exp) = tokens_of(&v, now_ms())?;
    crate::app_tokens::set_oauth(&p.app, &access, refresh.as_deref(), exp, &p.client_id, &p.server.token, &p.server.resource)?; Ok(p.app)
}
/// Renew an app's access token when it is near its end. Quiet when the app has no sign-in of this kind or nothing is due.
pub async fn refresh_if_due(app: &str) {
    let Some(o) = crate::app_tokens::oauth_of(app) else { return }; if !renew_due(o["expires_at"].as_i64().unwrap_or(0), now_ms()) { return; }
    let Some(refresh) = o["refresh"].as_str().filter(|r| !r.is_empty()) else { return };
    let Ok(c) = http() else { return };
    let r = c.post(o["token_endpoint"].as_str().unwrap_or("")).header("accept", "application/json").form(&[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", o["client_id"].as_str().unwrap_or("")), ("resource", o["resource"].as_str().unwrap_or(""))]).send().await;
    let Ok(r) = r else { return }; let Ok(v) = r.json::<Value>().await else { return };
    if let Ok((access, newrefresh, exp)) = tokens_of(&v, now_ms()) { let _ = crate::app_tokens::set_oauth(app, &access, newrefresh.as_deref().or(Some(refresh)), exp, o["client_id"].as_str().unwrap_or(""), o["token_endpoint"].as_str().unwrap_or(""), o["resource"].as_str().unwrap_or("")); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_pkce_challenge_is_the_rfc_example_and_the_sign_in_address_carries_everything_the_server_needs() {
        assert_eq!(challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let u = authorize_url("https://x.io/authorize", "cid", "http://127.0.0.1:41999/cb", "chal", "st", "read write", "https://x.io/mcp");
        for p in ["response_type=code", "client_id=cid", "redirect_uri=http%3A%2F%2F127.0.0.1%3A41999%2Fcb", "code_challenge=chal", "code_challenge_method=S256", "state=st", "scope=read%20write", "resource=https%3A%2F%2Fx.io%2Fmcp"] { assert!(u.contains(p), "{p} in {u}"); }
        assert_ne!(random_text(24), random_text(24)); assert!(random_text(48).len() >= 60);
    }
    #[test]
    fn a_401_names_where_the_sign_in_is_described_and_a_stranger_address_is_not_followed() {
        assert_eq!(resource_metadata_of(r#"Bearer realm="OAuth", resource_metadata="https://mcp.linear.app/.well-known/oauth-protected-resource/mcp", scope="read write""#).as_deref(), Some("https://mcp.linear.app/.well-known/oauth-protected-resource/mcp"));
        assert_eq!(resource_metadata_of("Bearer realm=x"), None); assert_eq!(resource_metadata_of(r#"Bearer resource_metadata="file:///etc/passwd""#), None);
        assert_eq!(origin_of("https://mcp.linear.app/mcp"), "https://mcp.linear.app");
    }
    #[test]
    fn a_token_reply_is_read_with_its_end_and_an_error_reply_says_why_and_renewal_waits_for_the_last_minute() {
        let (a, r, exp) = tokens_of(&json!({ "access_token": "A", "refresh_token": "R", "expires_in": 100 }), 1_000).unwrap(); assert_eq!((a.as_str(), r.as_deref(), exp), ("A", Some("R"), 101_000));
        assert!(tokens_of(&json!({ "error": "invalid_grant", "error_description": "expired" }), 0).unwrap_err().contains("invalid_grant: expired")); assert!(tokens_of(&json!({}), 0).is_err());
        assert!(!renew_due(1_000_000, 0) && renew_due(1_000_000, 950_000) && !renew_due(0, 5), "no end known: never renewed");
    }
}
