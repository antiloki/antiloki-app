//! WHERE AN APP'S TOKEN LIVES (RFC 0009 §4.2): the person pastes it once; it is kept in a file in the machine's antiloki home with the owner's permission only, NEVER in the repo, a layout, a saga, the
//! trail of calls or any reply. Sign-in is per machine and per person. (The operating system's own keyring is the planned home; this is the file behind the same four functions, so moving it changes nothing else.)
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;
/// Every change is read-modify-write of ONE file, and a refresh of a sign-in (a background task) can run while the person pastes a token: without this lock the later write carried the earlier read and the pasted token vanished (2026-11-20, GitHub).
static LOCK: Mutex<()> = Mutex::new(());
fn locked<T>(f: impl FnOnce() -> T) -> T { let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner()); f() }

pub fn path() -> PathBuf {
    std::env::var("ANTILOKI_APP_TOKENS_FILE").ok().filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| antiloki_rs::cockpit__helpers_webapps::store_path().with_file_name("app-tokens.json"))
}
fn load() -> serde_json::Map<String, Value> { std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default() }
fn save(m: &serde_json::Map<String, Value>) -> Result<(), String> {
    let p = path(); if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let tmp = p.with_extension("json.tmp"); std::fs::write(&tmp, serde_json::to_string_pretty(&Value::Object(m.clone())).unwrap_or_default()).map_err(|e| e.to_string())?;   // written beside, then renamed: a reader never sees half a file
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?; }
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?; }
    Ok(())
}
pub fn get(app: &str) -> Option<String> { load().get(app).and_then(|v| v["token"].as_str()).filter(|t| !t.is_empty()).map(String::from) }
pub fn set(app: &str, token: &str) -> Result<(), String> { if token.trim().is_empty() { return Err("the token is empty".into()); } locked(|| { let mut m = load(); m.insert(app.to_string(), json!({ "token": token.trim() })); save(&m) }) }
pub fn forget(app: &str) -> Result<bool, String> { locked(|| { let mut m = load(); let had = m.remove(app).is_some(); save(&m)?; Ok(had) }) }
/// Is there a token for this app? (the only thing a reply may say about it)
pub fn has(app: &str) -> bool { get(app).is_some() }
/// A sign-in by the server's own OAuth (mcp_oauth): the access token goes where a pasted one would, so every caller uses it unchanged; the refresh token and what is needed to renew sit beside it.
pub fn set_oauth(app: &str, access: &str, refresh: Option<&str>, expires_at_ms: i64, client_id: &str, token_endpoint: &str, resource: &str) -> Result<(), String> {
    locked(|| { let mut m = load(); m.insert(app.to_string(), json!({ "token": access, "oauth": { "refresh": refresh.unwrap_or(""), "expires_at": expires_at_ms, "client_id": client_id, "token_endpoint": token_endpoint, "resource": resource } })); save(&m) })
}
pub fn oauth_of(app: &str) -> Option<Value> { load().get(app).and_then(|v| v.get("oauth")).cloned().filter(|o| o.is_object()) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn many_writers_at_once_lose_no_token() {
        let f = std::env::temp_dir().join(format!("app-tokens-race-{}.json", std::process::id())); let _ = std::fs::remove_file(&f); std::env::set_var("ANTILOKI_APP_TOKENS_FILE", &f);
        let hs: Vec<_> = (0..24).map(|i| std::thread::spawn(move || { if i % 2 == 0 { set(&format!("app{i}"), "tok").unwrap(); } else { set_oauth(&format!("app{i}"), "a", Some("r"), 1, "c", "t", "r").unwrap(); } })).collect();
        for h in hs { h.join().unwrap(); }
        for i in 0..24 { assert!(has(&format!("app{i}")), "app{i} was lost"); }
        let _ = std::fs::remove_file(&f);
    }
}
