//! WHAT IS RUNNING ON THIS MACHINE (the operator 2026-11-22: the browser pane "shows a pretty list of everything that is currently running on the ports"): every local port that answers, with the title of the page when it is a web page. One way for Linux and Windows alike: ask the port itself (a short connect), no `netstat`, no shell.
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// The ports to ask: all of the unprivileged ones.
pub fn candidate_ports() -> Vec<u16> { (1024..=65535).collect() }   // a closed port on this machine answers at once, so every unprivileged port is asked (a server can be on any of them)
/// The `<title>` of an HTML answer, trimmed. Pure.
pub fn title_of(body: &str) -> Option<String> {
    // a `<title` inside an HTML comment is not the page's title
    let mut clean = String::with_capacity(body.len()); let mut rest = body; while let Some(a) = rest.find("<!--") { clean.push_str(&rest[..a]); rest = match rest[a..].find("-->") { Some(b) => &rest[a + b + 3..], None => "" }; } clean.push_str(rest); let body = clean.as_str();
    let low = body.to_ascii_lowercase(); let a = low.find("<title")?; let a = low[a..].find('>')? + a + 1; let b = low[a..].find("</title")? + a;
    let t = body[a..b].split_whitespace().collect::<Vec<_>>().join(" "); if t.is_empty() || t.contains('<') || t.contains('>') { None } else { Some(t.chars().take(80).collect()) }
}
fn probe(port: u16) -> Option<Value> {
    // a dev server may listen on IPv4 only, IPv6 only (`localhost` is ::1 for some), or both: ask both
    let v4 = SocketAddr::from(([127, 0, 0, 1], port)); let v6 = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&v4, Duration::from_millis(120)).or_else(|_| TcpStream::connect_timeout(&v6, Duration::from_millis(120))).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(400))); let _ = s.set_write_timeout(Some(Duration::from_millis(400)));
    let mut title = None; let mut web = false;
    // only the usual web ports are spoken to; any other open port is listed as a service without sending it anything
    let speaks = (3000..=9999).contains(&port) || port == 11434 || port == 1313 || port == 1420;
    if speaks && s.write_all(format!("GET / HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nAccept: text/html\r\nConnection: close\r\n\r\n").as_bytes()).is_ok() {
        let mut buf = vec![0u8; 16384]; let n = s.read(&mut buf).unwrap_or(0); let head = String::from_utf8_lossy(&buf[..n]).to_string(); web = head.starts_with("HTTP/"); if web { title = title_of(&head); }
    }
    Some(json!({ "port": port, "url": format!("http://localhost:{port}/"), "web": web, "title": title }))
}
/// All of them, found in parallel, lowest port first. This is blocking: call it off the async threads.
pub fn scan() -> Vec<Value> {
    let ports = candidate_ports(); let chunk = ports.len().div_ceil(48); let mut hs = vec![];
    for c in ports.chunks(chunk) { let c = c.to_vec(); hs.push(std::thread::spawn(move || c.into_iter().filter_map(probe).collect::<Vec<_>>())); }
    let mut out: Vec<Value> = hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect(); out.sort_by_key(|v| v["port"].as_u64().unwrap_or(0)); out
}
/// `GET /api/v1/local/ports`.
pub async fn ports_get() -> Response {
    let r = tokio::task::spawn_blocking(scan).await.unwrap_or_default(); (StatusCode::OK, Json(json!({ "ok": true, "ports": r }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn the_title_of_a_page_is_read_plainly() { assert_eq!(title_of("HTTP/1.1 200 OK\r\n\r\n<html><head><TITLE>\n  My   app </TITLE>").as_deref(), Some("My app")); assert_eq!(title_of("no title here"), None); assert_eq!(title_of("<title></title>"), None); assert_eq!(title_of("<!-- <title>old</title> --><title>Real</title>").as_deref(), Some("Real")); }
    #[test] fn a_port_that_listens_is_found_with_its_title_and_one_that_does_not_is_not() {
        // probe() only speaks to the usual web ports (3000..=9999): bind one of those, not a random high one
        let (l, port) = (3100u16..9000).find_map(|p| std::net::TcpListener::bind(("127.0.0.1", p)).ok().map(|l| (l, p))).expect("a free port between 3100 and 9000");
        std::thread::spawn(move || { if let Ok((mut s, _)) = l.accept() { let mut b = [0u8; 512]; let _ = s.read(&mut b); let _ = s.write_all(b"HTTP/1.0 200 OK\r\n\r\n<title>Hello</title>"); } });
        let p = probe(port).expect("found"); assert_eq!(p["web"], json!(true)); assert_eq!(p["title"], json!("Hello")); assert!(probe(9).is_none());
    }
    #[test] fn the_candidates_are_the_unprivileged_ports_without_duplicates() { let c = candidate_ports(); assert!(c.contains(&3000) && c.contains(&5173) && c.contains(&8080) && c.contains(&41931) && !c.contains(&80)); let mut d = c.clone(); d.dedup(); assert_eq!(c.len(), d.len()); }
}
