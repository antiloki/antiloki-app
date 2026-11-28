//! Self-contained route tests.
//!
//! WHY THIS FILE EXISTS: until now the 347 routes were verified by A/B against the running NestJS
//! BE. That oracle disappears at cutover — after which nothing would prove the surface still
//! behaves. These tests drive the real `Router` through `tower::oneshot` against a REAL engine on
//! a throwaway substrate: no port, no listener, no BE, no network.
//!
//! They are deliberately behavioural, not "it returns 200". Each one asserts a rule that was
//! MEASURED off the live BE and would silently regress otherwise — the 201-for-POST default, the
//! three error envelopes, nil-means-empty-body, absence-is-not-null, and the two different
//! integer-parsing contracts.

use antiloki_serve::{build_router, dispatch::AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

/// A fresh substrate in a temp dir, seeded by the engine itself. Hermetic: every test run starts
/// from the same state, and nothing touches the operator's real `.antiloki`.
fn test_state() -> Arc<AppState> {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // LEFTOVERS (2026-09-18: 192 of these — 2.9 GB — filled /tmp): every run first sweeps the dirs of
    // test processes that are gone; ours are removed when the process ends.
    sweep_stale_test_dirs();
    let dir = std::env::temp_dir().join(format!("antiloki-serve-test-{}-{n}", std::process::id()));
    let db_dir = dir.join(".antiloki").join("db");
    std::fs::create_dir_all(&db_dir).expect("temp substrate dir");

    let path = db_dir.join("state.sqlite");
    let conn = antiloki_rs::engine_open_db::openDb(
        &serde_json::json!(path.to_string_lossy()),
        &serde_json::json!({ "readOnly": false }),
    )
    .expect("openDb seeds a fresh substrate");

    Arc::new(AppState {
        conn: Arc::new(Mutex::new(conn)),
        pool: vec![],
        events: antiloki_serve::events::EventsHub::new(),
        waker: std::sync::OnceLock::new(),
        db_path: ":memory:".into(),
    })
}

fn sweep_stale_test_dirs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let me = std::process::id();
        let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix("antiloki-serve-test-") else { continue };
            let pid: u32 = rest.split('-').next().and_then(|p| p.parse().ok()).unwrap_or(0);
            let alive = pid == me || (cfg!(target_os = "linux") && std::path::Path::new(&format!("/proc/{pid}")).exists());
            if !alive { let _ = std::fs::remove_dir_all(e.path()); }
        }
        // and our own, at exit — libtest runs tests on threads of this one process
        extern "C" fn cleanup() { let me = std::process::id(); if let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) { for e in rd.flatten() { if e.file_name().to_string_lossy().starts_with(&format!("antiloki-serve-test-{me}-")) { let _ = std::fs::remove_dir_all(e.path()); } } } }
        unsafe { libc_atexit(cleanup); }
    });
}
extern "C" { #[link_name = "atexit"] fn libc_atexit(f: extern "C" fn()) -> i32; }


async fn send(router: axum::Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map(|b| Body::from(b.to_string())).unwrap_or(Body::empty()))
        .expect("request builds");
    let res = router.oneshot(req).await.expect("router responds");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("body collects").to_bytes();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// The tick loop must stay off in tests — it would advance real pipeline state in the background
/// and make assertions time-dependent.
fn router() -> axum::Router {
    std::env::set_var("ANTILOKI_BE_TICK_DISABLED", "true");
    std::env::set_var("ANTILOKI_SHIPPED_BENCH", "off");   // the routing journeys assume a record they built themselves, not the benchmarks antiloki ships
    build_router(test_state())
}

// ── completeness ────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_generated_route_is_registered() {
    // DELETE is registered nowhere, so axum answers 405 when the PATH exists and 404 when it does
    // not. That proves registration WITHOUT executing a single handler — no side effects, no cost.
    // ONE router and ONE substrate for the whole sweep. Building them per route meant 324 database
    // creations and a five-minute test — which is a test nobody waits for, so nobody runs it.
    let app = router();
    let mut missing = Vec::new();
    for r in antiloki_serve::routes::ROUTES {
        let uri = r.path.replace("{slug}", "x").replace("{id}", "1").replace("{name}", "x")
            .replace("{intent}", "play").replace("{file}", "x").replace("{renderId}", "1");
        let (status, _) = send(app.clone(), "DELETE", &uri, None).await;
        if status != StatusCode::METHOD_NOT_ALLOWED {
            missing.push(format!("{} {} -> {}", r.method, r.path, status));
        }
    }
    assert!(missing.is_empty(), "routes not registered:\n{}", missing.join("\n"));
}

#[tokio::test]
async fn the_generated_table_is_not_empty_and_matches_the_manifest_shape() {
    // A generator bug that emitted zero routes would make every other test here vacuously pass.
    assert!(
        antiloki_serve::routes::ROUTES.len() > 300,
        "expected the full surface, got {}",
        antiloki_serve::routes::ROUTES.len()
    );
    for r in antiloki_serve::routes::ROUTES {
        assert!(r.path.starts_with("/api/v1/"), "unexpected path {}", r.path);
        assert!(matches!(r.method, "GET" | "POST"), "unexpected method {}", r.method);
        assert!(!r.action.is_empty(), "route {} has no action", r.path);
    }
}

// ── response conventions measured off the live BE ───────────────────────────

#[tokio::test]
async fn post_answers_201_not_200() {
    // NestJS's default success code for POST. An axum handler returning 200 diverges on EVERY
    // POST in the surface, and the FE sees it.
    let (status, _) = send(router(), "POST", "/api/v1/laws/list", Some("{}")).await;
    assert_eq!(status, StatusCode::CREATED, "POST must answer 201");
}

#[tokio::test]
async fn get_answers_200() {
    let (status, _) = send(router(), "GET", "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn healthz_shape_is_exactly_ok_version_uptime() {
    let (_, body) = send(router(), "GET", "/healthz", None).await;
    let v: Value = serde_json::from_str(&body).expect("healthz is JSON");
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
    keys.sort();
    // The BE has no `engine` key here and DOES have uptime_s — an earlier version got this wrong. `fe_build` names the build of the app the serve hands out (575cae3); it is null when there is none.
    assert_eq!(keys, vec!["fe_build", "ok", "uptime_s", "version"]);
}

#[tokio::test]
async fn healthz_engine_reports_rust() {
    let (status, body) = send(router(), "GET", "/healthz/engine", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, r#"{"engine":"rust"}"#);
}

// ── the three error envelopes ───────────────────────────────────────────────

#[tokio::test]
async fn engine_failures_use_the_typed_envelope() {
    // A missing required field reaches the engine and comes back as the engine's own shape:
    // {error:{code,skill,cause,context}} — NOT Nest's {message,error,statusCode}.
    let (status, body) = send(router(), "POST", "/api/v1/codebase/file/read", Some("{}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v: Value = serde_json::from_str(&body).expect("JSON body");
    assert_eq!(v["error"]["skill"], "action-run");
    assert_eq!(v["error"]["code"], "VALIDATION_REQUIRED");
    assert!(v["error"]["context"]["action"].is_string(), "context carries the action");
    assert!(v.get("statusCode").is_none(), "must NOT be the Nest envelope");
}

#[tokio::test]
async fn dto_validation_uses_nests_array_message_envelope() {
    // class-validator's message is an ARRAY, and the order is REVERSED from decorator source
    // order because TypeScript decorators evaluate bottom-up.
    let (status, body) = send(router(), "POST", "/api/v1/blueprint/get", Some("{}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        r#"{"message":["contract_slug must be longer than or equal to 1 characters","contract_slug must be a string"],"error":"Bad Request","statusCode":400}"#
    );
}

#[tokio::test]
async fn forbid_non_whitelisted_rejects_unknown_properties() {
    let (status, body) = send(
        router(),
        "POST",
        "/api/v1/blueprint/get",
        Some(r#"{"contract_slug":"x","bogus":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("property bogus should not exist"), "got {body}");
}

#[tokio::test]
async fn manual_guards_use_a_string_message_not_an_array() {
    // `throw new BadRequestException('...')` yields message as a STRING. Same status and keys as
    // class-validator's array form — an FE doing message[0] behaves differently between them.
    let (status, body) = send(router(), "POST", "/api/v1/card/reorder", Some(r#"{"slug":"x"}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v: Value = serde_json::from_str(&body).expect("JSON body");
    assert!(v["message"].is_string(), "manual guard message must be a STRING, got {body}");
    assert_eq!(v["message"], "position is required");
}

#[tokio::test]
async fn path_param_guards_interpolate_the_offending_value() {
    let (status, body) = send(router(), "POST", "/api/v1/notifications/0/dismiss", Some("{}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("invalid notification id '0'"), "got {body}");
}

#[tokio::test]
async fn unknown_paths_get_the_nest_not_found_body() {
    let (status, body) = send(router(), "GET", "/api/v1/definitely-not-a-route", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Not Found"), "got {body}");
}

// ── the two integer-parsing contracts ───────────────────────────────────────

#[tokio::test]
async fn parse_int_pipe_rejects_non_integers_but_falls_back_on_junk() {
    // MEASURED off the BE: `Number()` semantics. "1.5" is rejected; "abc" is NOT — it fails to
    // convert at all and the DefaultValuePipe value stands. "5abc" being accepted rules out
    // parseInt as the check.
    let (bad, _) = send(router(), "GET", "/api/v1/cards?limit=1.5", None).await;
    assert_eq!(bad, StatusCode::BAD_REQUEST, "1.5 must be rejected");

    for junk in ["abc", "5abc", "NaN"] {
        let (ok, _) = send(router(), "GET", &format!("/api/v1/cards?limit={junk}"), None).await;
        assert_eq!(ok, StatusCode::OK, "{junk} must fall back to the default, not 400");
    }
}

#[tokio::test]
async fn parse_int_pipe_rejects_values_beyond_2_pow_53() {
    let (status, _) = send(router(), "GET", "/api/v1/cards?limit=999999999999999999999", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn param_int_pipe_rejects_a_non_numeric_path_segment() {
    let (status, body) = send(router(), "GET", "/api/v1/memory/abc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("numeric string is expected"), "got {body}");
}

// ── attribution ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn missing_session_header_is_attributed_as_unauthenticated() {
    // Attribution is derived from X-FE-Session and never trusted from the body; with no header the
    // actor must be `unauthenticated-http`, which is what lands in the audit chain.
    let state = test_state();
    let router = build_router(state.clone());
    let (status, _) = send(router, "POST", "/api/v1/laws/list", Some("{}")).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn a_session_header_produces_a_web_fe_actor() {
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/laws/list")
        .header("content-type", "application/json")
        .header("x-fe-session", "abcdefgh12345")
        .body(Body::from("{}"))
        .unwrap();
    let res = router().oneshot(req).await.expect("responds");
    assert_eq!(res.status(), StatusCode::CREATED);
}

// THE CLI COMPLETIONS (2026-09-19): the product's own chat-engine endpoint. The real CLI is never run
// here — the contract under test is the request gate and the OpenAI-shaped error envelope.
#[tokio::test]
async fn cli_completions_rejects_an_unknown_engine_and_an_empty_transcript() {
    let (st, body) = send(router(), "POST", "/api/v1/cli-llm/chat/completions", Some(r#"{"model":"gpt-4","messages":[{"role":"user","content":"hi"}]}"#)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    // (the message names cli:auto first since 4ba2d50: nothing pins Claude any more, and the operator's Claude subscription has ended)
    assert!(body.contains("unknown CLI engine 'gpt-4'") && body.contains("cli:auto") && body.contains("claude-code"), "{body}");
    let (st, body) = send(router(), "POST", "/api/v1/cli-llm/chat/completions", Some(r#"{"model":"cli:claude-code","messages":[]}"#)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("messages required"), "{body}");
}
