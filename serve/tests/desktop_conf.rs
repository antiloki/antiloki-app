//! THE DESKTOP APP'S SECURITY POLICY (operator 2026-10-11, Windows: the preview said "This content is blocked. Contact the site owner to fix the issue."). The packaged app has a Content-Security-Policy of
//! `default-src 'self'`, and a frame falls back to it when no `frame-src` is named - so the preview's `http://localhost:<port>` page (the workspace's own dev server, on a port nobody can list in advance) was
//! refused. In `vite dev` there is no policy, so it only ever failed in the installed app. This pins the fix: loopback pages may be framed, and nothing else is opened up.
fn csp() -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tauri/tauri.conf.json");
    let c: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).expect("tauri.conf.json")).expect("valid json");
    c["app"]["security"]["csp"].as_str().expect("a csp").to_string()
}
fn directive(csp: &str, name: &str) -> Vec<String> { csp.split(';').map(str::trim).find(|d| d.starts_with(name)).map(|d| d.split_whitespace().skip(1).map(String::from).collect()).unwrap_or_default() }

#[test]
fn the_preview_may_be_framed_from_loopback_and_only_from_loopback() {
    let c = csp();
    let f = directive(&c, "frame-src");
    assert!(f.contains(&"http://127.0.0.1:*".to_string()) && f.contains(&"http://localhost:*".to_string()), "frame-src must allow the preview's loopback ports: {c}");
    assert!(f.iter().all(|s| s == "'self'" || s.starts_with("http://127.0.0.1:") || s.starts_with("http://localhost:")), "nothing but loopback may be framed: {f:?}");
}
#[test]
fn the_rest_of_the_policy_stays_closed() {
    let c = csp();
    assert!(c.contains("default-src 'self'"), "{c}");
    assert!(!c.contains("default-src *") && !c.contains("script-src *") && !c.contains("'unsafe-eval'"), "{c}");
    assert!(directive(&c, "connect-src").contains(&"http://127.0.0.1:41931".to_string()), "the app still talks to its own serve: {c}");
}
