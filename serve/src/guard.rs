//! THE LOCAL DOOR (audit 2026-10-01, F-01): the serve listens on 127.0.0.1 and used to answer EVERY web page the operator
//! visits. `CorsLayer::allow_origin(Any)` + writes with outward effects (`deliver/publish` runs `gh repo create --push`, the
//! forge-token routes, QA replay/remove) + bodies parsed regardless of Content-Type = a `text/plain` cross-site POST needs
//! no preflight, so any page could POST `{pane_id:N, confirm:true}` to 127.0.0.1:41931; DNS rebinding did the same, and with
//! CORS `Any` the page could READ the GET answers too. Three rules close it, none of which touches a caller that sends no
//! `Origin` (curl, the CLIs, the desktop shell's own calls, the test suites):
//!
//! 1. CORS answers only the app's own origins (`origin_allowed`): `http(s)://localhost|127.0.0.1|[::1]` on any port,
//!    `tauri://localhost`, `http(s)://tauri.localhost`. A page from anywhere else cannot read an answer.
//! 2. A request that is not GET/HEAD/OPTIONS and carries an `Origin` outside that list is refused (403) before it reaches a
//!    handler - this is what stops the `text/plain` form POST (a browser always sends `Origin` on a cross-site POST, and the
//!    value `null` of a sandboxed frame is outside the list too). Content-Type is NOT required to be JSON: the handlers parse
//!    the body whatever it says (the CLIs rely on it), and the Origin rule above is what actually decides for a browser.
//! 3. ANY request whose `Host` is not `localhost` / `127.0.0.1` / `[::1]` is refused (403): a rebinding page reaches the
//!    serve under ITS OWN name. A request with no `Host` at all (HTTP/1.0, an in-process `oneshot`) passes.
use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

/// `localhost`, `127.0.0.1` or `[::1]` - the only names this serve is ever reached under.
fn loopback_name(h: &str) -> bool { matches!(h.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "[::1]") }

/// `host[:port]` (a Host header, or the authority of an Origin) -> (host, Some(port) | None). Strict: no userinfo, no path, a numeric port.
fn split_authority(a: &str) -> Option<(&str, Option<u16>)> {
    if a.is_empty() || a.contains(['/', '@', '\\', ' ', '?', '#']) { return None }
    let (host, port) = if let Some(rest) = a.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &a[..end + 2];
        let tail = &rest[end + 1..];
        (host, if tail.is_empty() { None } else { Some(tail.strip_prefix(':')?) })
    } else {
        match a.rsplit_once(':') { Some((h, p)) => (h, Some(p)), None => (a, None) }
    };
    let port = match port { None => None, Some(p) => Some(p.parse::<u16>().ok()?) };
    Some((host, port))
}

/// Is this `Host` header one of the serve's own loopback names? Pure - tested.
pub fn host_allowed(host: &str) -> bool { split_authority(host.trim()).map_or(false, |(h, _)| loopback_name(h)) }

/// Is this `Origin` one of the app's own? Pure - tested.
pub fn origin_allowed(origin: &str) -> bool {
    let o = origin.trim();
    if o == "tauri://localhost" { return true }
    let Some((scheme, rest)) = o.split_once("://") else { return false };
    let Some((host, _port)) = split_authority(rest) else { return false };
    match scheme.to_ascii_lowercase().as_str() {
        "http" => loopback_name(host) || host.eq_ignore_ascii_case("tauri.localhost"),
        "https" => host.eq_ignore_ascii_case("tauri.localhost"),
        "tauri" => host.eq_ignore_ascii_case("localhost"),
        _ => false,
    }
}

/// The CORS layer: the app's own origins may read answers; nobody else.
pub fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _| origin.to_str().map_or(false, origin_allowed)))
        .allow_headers(Any)
        .allow_methods(Any)
}

fn refuse(why: &str) -> Response {
    (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({ "statusCode": 403, "message": why }))).into_response()
}

/// Why this request must not be served, or None. Pure over the method and headers - tested.
pub fn verdict(method: &Method, headers: &HeaderMap) -> Option<&'static str> {
    if let Some(h) = headers.get(header::HOST) {
        if !h.to_str().map_or(false, host_allowed) { return Some("this server answers only to localhost, 127.0.0.1 and [::1] (a request under another name is refused)") }
    }
    let safe = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe {
        if let Some(o) = headers.get(header::ORIGIN) {
            if !o.to_str().map_or(false, origin_allowed) { return Some("a web page from another origin may not change anything here (the request's Origin is not one of this app's own)") }
        }
    }
    None
}

/// The middleware: `verdict`, then the handler.
pub async fn guard(req: Request<Body>, next: Next) -> Response {
    // TWO DOORS (remote.rs): a request that is not plainly local - another peer, another Host, a proxy header - is a device's or a stranger's, and is decided by its token; everything else keeps the rules above
    let peer = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip());
    if crate::remote::door(peer, req.headers()) == crate::remote::Door::Remote {
        if let Err(r) = crate::remote::gate_remote(req.method(), req.uri().path(), req.headers()) { return r }
        return next.run(req).await;
    }
    if let Some(why) = verdict(req.method(), req.headers()) { return refuse(why) }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::{get, post}, Router};
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/w", post(|body: axum::body::Bytes| async move { format!("did {}", body.len()) }))
            .route("/r", get(|| async { "read" })).route("/api/r", get(|| async { "read" }))
            .layer(axum::middleware::from_fn(guard))
            .layer(cors_layer())
    }
    async fn send(method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> (StatusCode, HeaderMap) {
        let mut b = Request::builder().method(method).uri(uri);
        for (k, v) in headers { b = b.header(*k, *v); }
        let res = app().oneshot(b.body(Body::from(body.to_string())).unwrap()).await.unwrap();
        (res.status(), res.headers().clone())
    }

    #[test]
    fn the_apps_own_origins_pass_and_every_other_is_refused() {
        for ok in ["http://localhost:5173", "http://127.0.0.1:41931", "http://[::1]:3000", "http://localhost", "tauri://localhost", "http://tauri.localhost", "https://tauri.localhost", "HTTP://LOCALHOST:1"] {
            assert!(origin_allowed(ok), "{ok}");
        }
        for bad in ["http://evil.com", "https://localhost.evil.com", "http://localhost.evil.com:80", "http://localhost@evil.com", "http://evil.com/localhost", "null", "", "file://", "https://localhost:5173",
                    "http://127.0.0.1.evil.net", "http://localhost:notaport", "ftp://localhost", "http://evil.com:80@localhost", "http://[::1", "tauri://evil"] {
            assert!(!origin_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn only_loopback_hosts_pass() {
        for ok in ["localhost", "localhost:41931", "127.0.0.1:41931", "[::1]:41931", "[::1]", "LocalHost:80"] { assert!(host_allowed(ok), "{ok}"); }
        for bad in ["evil.com", "evil.com:41931", "localhost.evil.com:41931", "127.0.0.1.evil.net", "192.168.1.5:41931", "0.0.0.0:41931", "localhost@evil.com", "", "[::2]:1"] { assert!(!host_allowed(bad), "{bad}"); }
    }

    #[tokio::test]
    async fn a_cross_site_text_plain_post_is_refused_before_a_handler_runs() {
        // the audit's exploit: no preflight needed, the page just posts the confirm
        let (st, _) = send("POST", "/w", &[("origin", "http://evil.com"), ("content-type", "text/plain"), ("host", "127.0.0.1:41931")], r#"{"pane_id":1,"confirm":true}"#).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let (st, _) = send("POST", "/w", &[("origin", "null"), ("content-type", "text/plain")], "{}").await;
        assert_eq!(st, StatusCode::FORBIDDEN, "a sandboxed frame's Origin: null");
    }

    #[tokio::test]
    async fn the_apps_own_origin_posts_and_reads_its_answers() {
        let (st, h) = send("POST", "/w", &[("origin", "http://localhost:5173"), ("content-type", "application/json"), ("host", "127.0.0.1:41931")], "{}").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(h.get("access-control-allow-origin").and_then(|v| v.to_str().ok()), Some("http://localhost:5173"));
        let (st, h) = send("GET", "/r", &[("origin", "tauri://localhost")], "").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(h.get("access-control-allow-origin").and_then(|v| v.to_str().ok()), Some("tauri://localhost"));
    }

    #[tokio::test]
    async fn an_evil_page_cannot_read_an_answer() {
        let (st, h) = send("GET", "/r", &[("origin", "http://evil.com")], "").await;
        assert_eq!(st, StatusCode::OK, "a GET is served (curl-like) but the browser gets no permission to read it");
        assert!(h.get("access-control-allow-origin").is_none(), "{h:?}");
        let (_, h) = send("OPTIONS", "/w", &[("origin", "http://evil.com"), ("access-control-request-method", "POST")], "").await;
        assert!(h.get("access-control-allow-origin").is_none(), "the preflight is not granted: {h:?}");
    }

    #[tokio::test]
    async fn a_request_under_another_host_name_is_refused_even_when_it_is_a_get() {
        // DNS rebinding: the page is evil.com, the address resolves to 127.0.0.1, so the browser sends no cross-origin marker at all.
        // Since the remote door (remote.rs) such a request is a STRANGER's: it may load the app's own files and nothing of the API, and a write is refused, until it carries a paired device's token.
        let (st, _) = send("GET", "/api/r", &[("host", "evil.com:41931")], "").await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, _) = send("POST", "/w", &[("host", "evil.com:41931"), ("content-type", "text/plain")], "{}").await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, _) = send("GET", "/api/r", &[("host", "localhost.evil.com")], "").await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, _) = send("GET", "/api/r", &[("host", "localhost"), ("x-forwarded-for", "9.9.9.9")], "").await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "a proxy header is a stranger even under a loopback name");
        let (st, _) = send("GET", "/api/r", &[("host", "127.0.0.1:41931")], "").await;
        assert_eq!(st, StatusCode::OK, "the local door is as it was");
    }

    #[tokio::test]
    async fn callers_with_no_origin_keep_working() {
        // curl, the CLIs, the desktop shell, the test suites: no Origin; text/plain or no content type at all is parsed as before
        for ct in [Some("application/json"), Some("text/plain"), None] {
            let mut h = vec![("host", "127.0.0.1:41931")];
            if let Some(c) = ct { h.push(("content-type", c)); }
            let (st, _) = send("POST", "/w", &h, "{}").await;
            assert_eq!(st, StatusCode::OK, "{ct:?}");
        }
        let (st, _) = send("POST", "/w", &[], "{}").await;
        assert_eq!(st, StatusCode::OK, "an in-process request carries no Host either");
    }
}
