//! THE REQUEST METER — `ANTILOKI_REQ_METER=1` and the serve prints, every 60 s, the paths the FE hit,
//! how many times and how long they took in total. Off by default: one atomic check per request.
use axum::{body::Body, http::Request, middleware::Next, response::Response};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Meter { on: bool, since: Instant, rows: HashMap<String, (u64, u128)> }
fn state() -> &'static Mutex<Meter> {
    static M: OnceLock<Mutex<Meter>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Meter { on: std::env::var("ANTILOKI_REQ_METER").map(|v| v == "1").unwrap_or(false), since: Instant::now(), rows: HashMap::new() }))
}

pub async fn meter(req: Request<Body>, next: Next) -> Response {
    let on = state().lock().map(|m| m.on).unwrap_or(false);
    if !on { return next.run(req).await; }
    let path = req.uri().path().to_string();
    let t0 = Instant::now();
    let res = next.run(req).await;
    let ms = t0.elapsed().as_millis();
    if let Ok(mut m) = state().lock() {
        let e = m.rows.entry(path).or_insert((0, 0)); e.0 += 1; e.1 += ms;
        if m.since.elapsed().as_secs() >= 60 {
            let mut rows: Vec<(String, (u64, u128))> = m.rows.drain().collect();
            rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
            let total_calls: u64 = rows.iter().map(|r| r.1 .0).sum();
            let total_ms: u128 = rows.iter().map(|r| r.1 .1).sum();
            eprintln!("antiloki-serve: requests in the last {}s — {} calls · {} ms busy", m.since.elapsed().as_secs(), total_calls, total_ms);
            for (p, (n, t)) in rows.iter().take(14) { eprintln!("  {:>5} × {:<52} {:>7} ms", n, p, t); }
            m.since = Instant::now();
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};
    use tower::ServiceExt;

    // The meter's switch and table are ONE process-wide static (read from the env once, at first use). These tests flip
    // the switch directly and use paths of their own, so they neither depend on the environment nor disturb anything
    // else that passes through the same layer; the lock keeps them from flipping it under each other.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn app() -> Router {
        Router::new()
            .route("/meter-test/fast", get(|| async { "ok" }))
            .route("/meter-test/slow", get(|| async { tokio::time::sleep(std::time::Duration::from_millis(40)).await; "ok" }))
            .layer(axum::middleware::from_fn(meter))
    }
    async fn hit(app: &Router, uri: &str) -> axum::http::StatusCode {
        app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap().status()
    }
    fn row(path: &str) -> Option<(u64, u128)> { state().lock().unwrap().rows.get(path).copied() }
    fn set_on(on: bool) { state().lock().unwrap().on = on; }

    /// RULE: off by default - the layer is a pass-through that records nothing (one atomic check per request).
    #[tokio::test]
    async fn switched_off_it_answers_and_records_nothing() {
        let _g = SERIAL.lock().await;
        set_on(false);
        state().lock().unwrap().rows.remove("/meter-test/fast");
        assert_eq!(hit(&app(), "/meter-test/fast").await, axum::http::StatusCode::OK);
        assert_eq!(row("/meter-test/fast"), None);
    }

    /// RULE: switched on, it counts calls per PATH (never per query string) and adds up the time they took.
    #[tokio::test]
    async fn switched_on_it_counts_calls_per_path_and_adds_up_their_time() {
        let _g = SERIAL.lock().await;
        set_on(true);
        { let mut m = state().lock().unwrap(); m.rows.remove("/meter-test/fast"); m.rows.remove("/meter-test/slow"); m.since = Instant::now(); }
        let a = app();
        for q in ["/meter-test/fast", "/meter-test/fast?x=1", "/meter-test/fast?x=2&y=3"] { assert_eq!(hit(&a, q).await, axum::http::StatusCode::OK); }
        assert_eq!(hit(&a, "/meter-test/slow").await, axum::http::StatusCode::OK);
        assert_eq!(row("/meter-test/fast").unwrap().0, 3, "three calls, one row: the query string is not part of the key");
        let (n, ms) = row("/meter-test/slow").unwrap();
        assert_eq!(n, 1);
        assert!(ms >= 40, "the handler slept 40 ms, the meter saw {ms} ms");
        // an unknown path is still a call worth counting (and the layer does not change the answer)
        assert_eq!(hit(&a, "/meter-test/nope").await, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(row("/meter-test/nope").unwrap().0, 1);
        set_on(false);
    }

    /// RULE: every 60 s the table is reported and emptied, so it cannot grow for the life of the process.
    #[tokio::test]
    async fn after_a_minute_the_table_is_drained_and_the_window_restarts() {
        let _g = SERIAL.lock().await;
        set_on(true);
        {
            let mut m = state().lock().unwrap();
            m.rows.insert("/meter-test/old".into(), (5, 100));
            m.since = Instant::now().checked_sub(std::time::Duration::from_secs(61)).unwrap();
        }
        assert_eq!(hit(&app(), "/meter-test/fast").await, axum::http::StatusCode::OK);
        let m = state().lock().unwrap();
        assert!(m.rows.get("/meter-test/old").is_none() && m.rows.get("/meter-test/fast").is_none(), "the report took every row, including the request that triggered it");
        assert!(m.since.elapsed().as_secs() < 5, "and a new window began");
        drop(m);
        // inside the minute nothing is drained
        assert_eq!(hit(&app(), "/meter-test/fast").await, axum::http::StatusCode::OK);
        assert_eq!(row("/meter-test/fast").unwrap().0, 1);
        set_on(false);
    }
}
