//! THE TELEGRAM NODE (BL-19): a team sends a message to a Telegram chat or channel through the Bot API: `use_tool` type `telegram`, tool `send`. It is OUTWARD (it tells someone else), so the validator already demands a human
//! gate on every way to it and the node lists `network` and `outward`. The bot token is a NAME in the token store (the person stores it once, it is never written to a graph, a transcript or an output); the message is a summary
//! and links, cut to a size Telegram accepts, with link previews off. Debug builds can point at a fake Bot API (`ANTILOKI_TELEGRAM_API`); a release build always talks to api.telegram.org.
use serde_json::{json, Value};

pub const MAX_CHARS: usize = 3500;
/// A chat target: a numeric id (`123456`, `-100123456` for a channel or group) or a public `@channel_name`. Pure.
pub fn clean_chat(s: &str) -> Result<String, String> {
    let s = s.trim(); let numeric = { let d = s.strip_prefix('-').unwrap_or(s); !d.is_empty() && d.len() <= 20 && d.chars().all(|c| c.is_ascii_digit()) };
    let handle = s.strip_prefix('@').map_or(false, |h| (4..=32).contains(&h.len()) && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    if numeric || handle { Ok(s.to_string()) } else { Err(format!("`{s}` is not a chat: a number (-100... for a channel) or @channel_name")) }
}
/// The text to send: placeholders already filled by the caller; cut at a character boundary, never empty. Pure.
pub fn clean_text(s: &str) -> Result<String, String> { let t: String = s.trim().chars().take(MAX_CHARS).collect(); if t.is_empty() { Err("a message needs text".into()) } else { Ok(t) } }
fn api_base() -> String { if cfg!(debug_assertions) { std::env::var("ANTILOKI_TELEGRAM_API").ok().filter(|u| u.starts_with("http")).unwrap_or_else(|| "https://api.telegram.org".into()) } else { "https://api.telegram.org".into() } }
/// Send one message. The error never contains the token (the library's error text is cut to its reason).
pub async fn send(token: &str, chat: &str, text: &str) -> Result<Value, String> {
    let (chat, text) = (clean_chat(chat)?, clean_text(text)?); if token.trim().is_empty() { return Err("no bot token is stored under that name".into()); }
    let url = format!("{}/bot{}/sendMessage", api_base().trim_end_matches('/'), token.trim());
    let c = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).redirect(reqwest::redirect::Policy::none()).build().map_err(|e| e.to_string())?;
    let r = c.post(url).json(&json!({ "chat_id": chat, "text": text, "disable_web_page_preview": true })).send().await.map_err(|e| format!("Telegram could not be reached: {}", e.without_url()))?;
    let status = r.status().as_u16(); let v: Value = r.json().await.unwrap_or(Value::Null);
    if status == 200 && v["ok"] == json!(true) { Ok(json!({ "ok": true, "message_id": v["result"]["message_id"], "chat": chat, "chars": text.chars().count(), "text": format!("sent to {chat}") })) }
    else { Err(format!("Telegram said {status}: {}", v["description"].as_str().unwrap_or("no reason given").chars().take(200).collect::<String>())) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_chat_and_a_text_are_checked_before_anything_is_sent() {
        for ok in ["123456", "-100123456789", "@my_channel"] { assert!(clean_chat(ok).is_ok(), "{ok}"); }
        for bad in ["", "abc", "@ab", "@has space", "12 34", "-", "https://t.me/x", "@x;y"] { assert!(clean_chat(bad).is_err(), "{bad:?}"); }
        assert_eq!(clean_text("  hi  ").unwrap(), "hi"); assert!(clean_text("   ").is_err()); assert_eq!(clean_text(&"é".repeat(5000)).unwrap().chars().count(), MAX_CHARS);
    }
    #[tokio::test]
    async fn an_empty_token_or_a_bad_chat_fails_without_a_request() { assert!(send("", "123", "x").await.unwrap_err().contains("token")); assert!(send("t", "nope", "x").await.is_err()); }
}
