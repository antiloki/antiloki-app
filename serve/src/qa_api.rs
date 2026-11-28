//! API EVIDENCE - THE BACKEND'S VERSION OF THE QA FILM (operator 2026-09-30: *"if the task i asked is a backend task is there a way to make also the QA test and
//! display something similar to what the frontend did?"*).
//!
//! The frontend QA opens the page, does what the claim says and lets the BROWSER decide with `expect` steps. This is the same idea with no browser: the QA writes a short
//! API test - requests, and what each answer must contain - and a deterministic runner sends them to the box's own server (booted by `qa_evidence::DevServer`) and decides
//! pass or fail itself. No model looks at anything to get the verdict. The steps stream to the same live viewer as the film (`Live`), as a terminal: what was asked, what
//! came back, what was checked.
//!
//! A plan is DATA from a model, so it is validated and bounded like the browser's: a fixed set of actions, a step budget, relative paths only (the runner talks to the box's
//! server and to nothing else), and a capped body.
use crate::qa_evidence::{Evidence, Live};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const MAX_API_STEPS: usize = 24;
const BODY_CAP: usize = 200_000;

#[derive(Clone, Debug, PartialEq)]
pub enum ApiStep {
    /// `path` starts with `/` and is resolved against the box's server
    Request { method: String, path: String, body: Option<String> },
    /// checks the LAST response; every condition given must hold
    Expect(ApiExpect),
}

/// What the last response must look like. Every field is optional; all the given ones must hold (`not` inverts the whole).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ApiExpect {
    pub status: Option<u16>,
    /// a dotted path into the JSON body: `results.0.name`
    pub json: Option<String>,
    /// with `json`: the value at the path equals this
    pub equals: Option<Value>,
    /// the value at `json` (or the whole body) contains this text / array element
    pub contains: Option<String>,
    /// with `json`: the array (or string) at the path has at least this many items
    pub min_len: Option<u64>,
    /// with `json`: the path exists (true) or is absent (false)
    pub exists: Option<bool>,
    pub not: bool,
}
impl ApiExpect {
    /// the check in words - the film's caption and the evidence block
    pub fn words(&self) -> String {
        let mut p: Vec<String> = vec![];
        if let Some(s) = self.status { p.push(format!("the status is {s}")); }
        let at = self.json.clone().map(|j| format!("`{j}`")).unwrap_or_else(|| "the body".into());
        if let Some(e) = &self.equals { p.push(format!("{at} equals {e}")); }
        if let Some(c) = &self.contains { p.push(format!("{at} contains \"{c}\"")); }
        if let Some(n) = self.min_len { p.push(format!("{at} has at least {n} item(s)")); }
        match self.exists { Some(true) => p.push(format!("{at} exists")), Some(false) => p.push(format!("{at} is absent")), None => {} }
        let s = if p.is_empty() { "the response is there".to_string() } else { p.join(" and ") };
        if self.not { format!("NOT ({s})") } else { s }
    }
}

/// The QA's plan (a JSON array) -> steps, or why it is not a plan. Pure - tested.
pub fn parse_api_plan(plan: &Value) -> Result<Vec<ApiStep>, String> {
    let arr = plan.as_array().ok_or("a plan is a JSON array of steps")?;
    if arr.is_empty() { return Err("the plan has no steps".into()) }
    if arr.len() > MAX_API_STEPS { return Err(format!("the plan has {} steps; the budget is {MAX_API_STEPS}", arr.len())) }
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(String::from);
    let mut out = vec![];
    for (i, st) in arr.iter().enumerate() {
        let at = |m: &str| format!("step {}: {m}", i + 1);
        let act = s(st, "do").ok_or_else(|| at("needs a \"do\""))?.to_lowercase();
        out.push(match act.as_str() {
            "request" | "call" | "get" | "post" | "put" | "patch" | "delete" => {
                let method = if matches!(act.as_str(), "request" | "call") { s(st, "method").unwrap_or_else(|| "GET".into()).to_uppercase() } else { act.to_uppercase() };
                if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD") { return Err(at(&format!("method {method} is not allowed (GET, POST, PUT, PATCH, DELETE, HEAD)"))) }
                let path = s(st, "path").or_else(|| s(st, "url")).ok_or_else(|| at("a request needs a \"path\""))?;
                if !path.starts_with('/') || path.starts_with("//") || path.contains("://") { return Err(at("the path is relative to the box's own server and starts with a single /")) }
                let body = match st.get("body") { None | Some(Value::Null) => None, Some(Value::String(t)) => Some(t.clone()), Some(v) => Some(v.to_string()) };
                if body.as_ref().map_or(false, |b| b.len() > 20_000) { return Err(at("the body is capped at 20 000 characters")) }
                ApiStep::Request { method, path, body }
            }
            "expect" | "expect_response" | "expect_status" => {
                let e = ApiExpect {
                    status: st.get("status").and_then(Value::as_u64).map(|x| x.min(999) as u16),
                    json: s(st, "json").or_else(|| s(st, "path")),
                    equals: st.get("equals").cloned().filter(|v| !v.is_null()),
                    contains: s(st, "contains").or_else(|| s(st, "text")),
                    min_len: st.get("min_len").and_then(Value::as_u64),
                    exists: st.get("exists").and_then(Value::as_bool),
                    not: st.get("not").and_then(Value::as_bool).unwrap_or(false),
                };
                if e.status.is_none() && e.equals.is_none() && e.contains.is_none() && e.min_len.is_none() && e.exists.is_none() { return Err(at("a check asks for something: status, equals, contains, min_len or exists")) }
                if (e.equals.is_some() || e.min_len.is_some() || e.exists.is_some()) && e.json.is_none() { return Err(at("equals, min_len and exists need a \"json\" path")) }
                ApiStep::Expect(e)
            }
            other => return Err(at(&format!("unknown action \"{other}\" (request, expect)"))),
        });
    }
    Ok(out)
}

/// The inverse of `parse_api_plan`: the API test as the JSON it is written in, so a test that passed can be kept and parsed back into the same steps. Pure - tested by the round trip.
pub fn api_steps_json(steps: &[ApiStep]) -> Value {
    Value::Array(steps.iter().map(|st| match st {
        ApiStep::Request { method, path, body } => { let mut o = json!({ "do": "request", "method": method, "path": path }); if let Some(b) = body { o["body"] = json!(b); } o }
        ApiStep::Expect(e) => {
            let mut o = json!({ "do": "expect", "not": e.not });
            if let Some(s) = e.status { o["status"] = json!(s); }
            if let Some(j) = &e.json { o["json"] = json!(j); }
            if let Some(v) = &e.equals { o["equals"] = v.clone(); }
            if let Some(c) = &e.contains { o["contains"] = json!(c); }
            if let Some(n) = e.min_len { o["min_len"] = json!(n); }
            if let Some(x) = e.exists { o["exists"] = json!(x); }
            o
        }
    }).collect())
}

/// what the last request got back
#[derive(Clone, Debug, Default)]
pub struct Response { pub status: u16, pub body: String, pub json: Option<Value>, pub ms: u64 }

/// the value at a dotted path: `results.0.name`. Pure - tested.
pub fn json_at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for part in path.split('.').filter(|p| !p.is_empty()) {
        cur = match cur { Value::Array(a) => a.get(part.parse::<usize>().ok()?)?, Value::Object(o) => o.get(part)?, _ => return None };
    }
    Some(cur)
}

/// Decide one check against one response: `(passed, what the response actually had)`. Pure - tested. The deterministic heart of the backend QA.
pub fn eval_expect(e: &ApiExpect, r: &Response) -> (bool, String) {
    let mut ok = true;
    let mut had: Vec<String> = vec![format!("status {}", r.status)];
    if let Some(s) = e.status { ok &= r.status == s; }
    let target: Option<Value> = match &e.json { Some(p) => r.json.as_ref().and_then(|j| json_at(j, p)).cloned(), None => None };
    if let Some(p) = &e.json {
        match &target { Some(v) => had.push(format!("`{p}` = {}", short(&v.to_string(), 120))), None => had.push(format!("`{p}` is absent")) }
        if let Some(want) = e.exists { ok &= target.is_some() == want; }
        if let Some(eq) = &e.equals { ok &= target.as_ref() == Some(eq); }
        if let Some(n) = e.min_len { ok &= match &target { Some(Value::Array(a)) => a.len() as u64 >= n, Some(Value::String(t)) => t.chars().count() as u64 >= n, Some(Value::Object(o)) => o.len() as u64 >= n, _ => false }; }
    }
    if let Some(c) = &e.contains {
        let hit = match (&e.json, &target) {
            (Some(_), Some(Value::Array(a))) => a.iter().any(|x| x.as_str().map_or_else(|| x.to_string().to_lowercase().contains(&c.to_lowercase()), |t| t.to_lowercase().contains(&c.to_lowercase()))),
            (Some(_), Some(Value::String(t))) => t.to_lowercase().contains(&c.to_lowercase()),
            (Some(_), Some(v)) => v.to_string().to_lowercase().contains(&c.to_lowercase()),
            (Some(_), None) => false,
            (None, _) => r.body.to_lowercase().contains(&c.to_lowercase()),
        };
        ok &= hit;
        if e.json.is_none() { had.push(format!("body: {}", short(&r.body.split_whitespace().collect::<Vec<_>>().join(" "), 140))); }
    }
    (ok != e.not, had.join("; "))
}
fn short(s: &str, n: usize) -> String { if s.chars().count() <= n { s.to_string() } else { format!("{}...", s.chars().take(n).collect::<String>()) } }

/// Cut `s` to at most `max` bytes without splitting a character (`str::floor_char_boundary` is not stable on the older toolchains this builds with). Pure - tested.
fn cap_at_boundary(s: &mut String, max: usize) {
    if s.len() <= max { return }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) { cut -= 1; }
    s.truncate(cut);
}

fn send(base: &str, method: &str, path: &str, body: &Option<String>) -> Result<Response, String> {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let t0 = std::time::Instant::now();
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).redirects(3).build();
    let mut req = agent.request(method, &url).set("accept", "application/json, text/plain, */*");
    if body.is_some() { req = req.set("content-type", "application/json"); }
    let res = match body { Some(b) => req.send_string(b), None => req.call() };
    let resp = match res { Ok(r) => r, Err(ureq::Error::Status(_, r)) => r, Err(e) => return Err(format!("{method} {path}: {e}")) };
    let status = resp.status();
    let mut text = resp.into_string().unwrap_or_default();
    // AUDIT 2026-10-01 F-12: `text.truncate(BODY_CAP)` PANICS when the cap falls inside a multi-byte character, and the body is whatever the agent-written server answered
    cap_at_boundary(&mut text, BODY_CAP);
    let json = serde_json::from_str::<Value>(&text).ok();
    Ok(Response { status, body: text, json, ms: t0.elapsed().as_millis() as u64 })
}

/// Run a plan against a running server. A step that cannot be done (the server is down, a check fails) is logged and counted; the run goes on. The evidence is the same
/// shape the browser's is (`log`, `checks`, `failed_steps`), so the judge's block and the UI treat both alike. With a `Live` the steps stream as they happen.
pub fn run_api_plan_live(base: &str, steps: &[ApiStep], out_dir: &Path, live: Option<Live>) -> Result<Evidence, String> {
    let mut ev = Evidence::default();
    let t0 = std::time::Instant::now();
    let timeline: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(vec![]));
    let emit = {
        let (live, timeline) = (live.clone(), timeline.clone());
        move |mut v: Value| { if let Some(l) = &live { v["t"] = json!(t0.elapsed().as_millis() as u64); timeline.lock().unwrap_or_else(|p| p.into_inner()).push(v.clone()); (l.sink)(v); } }
    };
    let mut last: Option<Response> = None;
    for (i, st) in steps.iter().enumerate() {
        let n = i + 1;
        if live.as_ref().map_or(false, |l| l.cancel.load(std::sync::atomic::Ordering::Relaxed)) { ev.log.push(format!("{n}. stopped by the operator - the rest of the test was not run")); break }
        let (kind, text) = match st {
            ApiStep::Request { method, path, .. } => ("request", format!("sending {method} {path}")),
            ApiStep::Expect(e) => ("expect", format!("checking that {}", e.words())),
        };
        if live.is_some() {
            emit(json!({ "type": "step", "n": n, "of": steps.len(), "kind": kind, "text": text, "rect": Value::Null, "viewport": [1280, 800] }));
            if let Some(l) = &live { let ms = (l.pace)(); if ms > 0 { std::thread::sleep(Duration::from_millis(ms)); } }
        }
        let done: Result<String, String> = match st {
            ApiStep::Request { method, path, body } => match send(base, method, path, body) {
                Ok(r) => {
                    emit(json!({ "type": "exchange", "n": n, "method": method, "path": path, "request_body": body, "status": r.status, "ms": r.ms, "body": short(&r.body, 700) }));
                    let m = format!("{method} {path} -> {} in {} ms", r.status, r.ms);
                    last = Some(r); Ok(m)
                }
                Err(e) => { last = None; Err(e) }
            },
            ApiStep::Expect(e) => match &last {
                None => { ev.checks.push((e.words(), false, "there was no response to check".into())); Err(format!("check FAILED: expected {} - there was no response to check", e.words())) }
                Some(r) => {
                    let (ok, had) = eval_expect(e, r);
                    ev.checks.push((e.words(), ok, had.clone()));
                    if ok { Ok(format!("check passed: {}", e.words())) } else { Err(format!("check FAILED: expected {} - the response had: {had}", e.words())) }
                }
            },
        };
        match done {
            Ok(m) => { emit(json!({ "type": "step-done", "n": n, "ok": true, "message": m })); ev.log.push(format!("{n}. {m}")) }
            Err(e) => { emit(json!({ "type": "step-done", "n": n, "ok": false, "message": e })); ev.failed_steps += 1; ev.log.push(format!("{n}. FAILED - {e}")) }
        }
        if let Some(l) = &live { let ms = (l.pace)() / 2; if ms > 0 { std::thread::sleep(Duration::from_millis(ms)); } }
    }
    emit(json!({ "type": "done", "shots": 0, "failed_steps": ev.failed_steps }));
    if let Some(l) = &live { if let Some(d) = &l.film_dir { let _ = std::fs::create_dir_all(d); let _ = std::fs::write(d.join("timeline.json"), Value::Array(timeline.lock().unwrap_or_else(|p| p.into_inner()).clone()).to_string()); } }
    let _ = out_dir;
    Ok(ev)
}
pub fn run_api_plan(base: &str, steps: &[ApiStep]) -> Result<Evidence, String> { run_api_plan_live(base, steps, Path::new(""), None) }

/// The QA's reply -> a plan: the first valid plan among the JSON values the reply holds (whole reply, fenced block, balanced array/object in the prose - `qa_evidence::json_candidates`).
/// `None` when there is none. Pure - tested.
pub fn api_plan_from_reply(reply: &str) -> Option<Vec<ApiStep>> {
    crate::qa_evidence::json_candidates(reply).iter().find_map(|v| parse_api_plan(v).ok())
}

/// WHY a reply is not an API plan, for the run's log. Pure - tested.
pub fn api_plan_problem(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() { return "the reply was empty".into() }
    let cands = crate::qa_evidence::json_candidates(t);
    match cands.iter().find(|v| v.as_array().map_or(false, |a| a.iter().any(Value::is_object))).or_else(|| cands.iter().find(|v| v.is_array())) {
        None => format!("the reply held no JSON array of requests (it began: {})", t.chars().take(120).collect::<String>().replace('\n', " ")),
        Some(v) => match parse_api_plan(v) { Err(e) => format!("the plan was refused: {e}"), Ok(_) => "the plan is valid".into() },
    }
}

/// Is this change a SERVER change - code that answers requests and is not a screen, a test or a document? Pure - tested.
pub fn touches_backend(files: &[String]) -> bool {
    const EXT: &[&str] = &["rs", "ts", "js", "mjs", "cjs", "py", "go", "java", "rb", "php", "cs", "kt"];
    files.iter().any(|f| {
        let p = f.to_ascii_lowercase();
        let ext = Path::new(&p).extension().and_then(|e| e.to_str()).unwrap_or("");
        EXT.contains(&ext) && !p.contains(".test.") && !p.contains(".spec.") && !p.contains("/__tests__/") && !p.contains("/test/") && !p.contains("/tests/") && !p.contains("/e2e/")
            && !p.ends_with(".d.ts") && !p.ends_with(".config.ts") && !p.ends_with(".config.js")
    }) && !crate::qa_evidence::touches_frontend(files)
}

pub fn prompt_block(ev: &Evidence) -> String {
    let mut s = String::from("API EVIDENCE (requests sent to the box's own running server by a deterministic runner; the CHECKS were decided by the runner, not by any model):\n");
    if !ev.checks.is_empty() {
        s.push_str(&format!("  {} check(s) passed, {} failed. A failed check is a FACT about what the server answered:\n", ev.checks.len() - ev.checks_failed(), ev.checks_failed()));
        for (what, ok, got) in &ev.checks { s.push_str(&format!("    {} {what}{}\n", if *ok { "PASS" } else { "FAIL" }, if *ok { String::new() } else { format!(" - the response had: {got}") })); }
    }
    for l in &ev.log { s.push_str(&format!("  {l}\n")); }
    if ev.checks.is_empty() { s.push_str("  (the plan made no check, so this shows what the server answered and decides nothing)\n"); }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(status: u16, body: Value) -> Response { Response { status, body: body.to_string(), json: Some(body), ms: 3 } }

    /// F-12 (audit 2026-10-01): a server that answers with non-ASCII text longer than the cap used to panic the runner (`String::truncate` off a char boundary).
    #[test]
    fn a_long_multi_byte_answer_is_cut_on_a_character_boundary_instead_of_panicking() {
        let mut s = "\u{20ac}".repeat(100_000);               // 3 bytes each: 300 000 bytes, and BODY_CAP (200 000) is 2 bytes into a character
        assert!(!s.is_char_boundary(BODY_CAP), "the premise: the cap falls inside a character");
        cap_at_boundary(&mut s, BODY_CAP);
        assert_eq!(s.len(), 199_998); assert!(s.chars().all(|c| c == '\u{20ac}'));
        let mut short = "h\u{e9}llo".to_string(); cap_at_boundary(&mut short, 100); assert_eq!(short, "h\u{e9}llo", "under the cap: untouched");
        let mut one = "\u{1F600}".to_string(); cap_at_boundary(&mut one, 2); assert_eq!(one, "", "a cap inside the very first character leaves nothing, not a panic");
    }

    #[test]
    fn the_runner_survives_a_server_that_answers_in_multi_byte_text() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048]; let _ = c.read(&mut buf);
            let body = "\u{20ac}".repeat(100_000);
            let _ = write!(c, "HTTP/1.1 200 OK\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", body.len(), body);
        });
        let r = send(&format!("http://127.0.0.1:{port}"), "GET", "/", &None).expect("a long non-ASCII answer is a response, not a crash");
        assert_eq!(r.status, 200); assert_eq!(r.body.len(), 199_998);
        server.join().unwrap();
    }

    // A SAVED API TEST IS THE SAME TEST (item 8d, 2026-10-01): kept as JSON and parsed back it must be the steps that ran.
    #[test]
    fn an_api_test_kept_as_json_parses_back_into_the_same_steps() {
        let steps = vec![
            ApiStep::Request { method: "GET".into(), path: "/api/hello".into(), body: None },
            ApiStep::Expect(ApiExpect { status: Some(200), json: Some("message".into()), contains: Some("hello".into()), ..Default::default() }),
            ApiStep::Request { method: "POST".into(), path: "/api/cart".into(), body: Some("{\"sku\":\"red-shoes\"}".into()) },
            ApiStep::Expect(ApiExpect { status: Some(201), json: Some("items".into()), min_len: Some(1), ..Default::default() }),
            ApiStep::Expect(ApiExpect { json: Some("items.0.sku".into()), equals: Some(json!("red-shoes")), exists: Some(true), ..Default::default() }),
            ApiStep::Request { method: "DELETE".into(), path: "/api/cart/1".into(), body: None },
            ApiStep::Expect(ApiExpect { status: Some(500), not: true, ..Default::default() }),
        ];
        let json = api_steps_json(&steps);
        assert_eq!(parse_api_plan(&json).expect("the kept test is a valid test"), steps, "{json}");
    }

    #[test]
    fn an_api_plan_is_validated_and_bounded_because_it_is_data_from_a_model() {
        let ok = parse_api_plan(&json!([{ "do": "request", "method": "post", "path": "/api/cart", "body": { "sku": "red-shoes" } }, { "do": "expect", "status": 201, "json": "items", "min_len": 1 },
            { "do": "get", "path": "/api/search?q=shoes" }, { "do": "expect", "json": "results.0.name", "contains": "shoes" }])).unwrap();
        assert_eq!(ok[0], ApiStep::Request { method: "POST".into(), path: "/api/cart".into(), body: Some("{\"sku\":\"red-shoes\"}".into()) });
        assert!(matches!(&ok[2], ApiStep::Request { method, .. } if method == "GET"));
        assert!(parse_api_plan(&json!([{ "do": "request", "path": "http://evil.example/x" }])).is_err(), "only the box's own server");
        assert!(parse_api_plan(&json!([{ "do": "request", "path": "//evil.example/x" }])).is_err());
        assert!(parse_api_plan(&json!([{ "do": "request", "path": "no-slash" }])).is_err());
        assert!(parse_api_plan(&json!([{ "do": "request", "method": "TRACE", "path": "/x" }])).is_err());
        assert!(parse_api_plan(&json!([{ "do": "expect" }])).is_err(), "a check must ask for something");
        assert!(parse_api_plan(&json!([{ "do": "expect", "min_len": 2 }])).is_err(), "min_len needs a json path");
        assert!(parse_api_plan(&json!([])).is_err() && parse_api_plan(&json!("x")).is_err());
        assert!(parse_api_plan(&Value::Array((0..30).map(|_| json!({ "do": "get", "path": "/" })).collect())).is_err(), "the step budget");
    }

    #[test]
    fn a_check_is_decided_by_the_response_alone() {
        let r = resp(200, json!({ "results": [{ "name": "Red Shoes", "price": 40 }, { "name": "Blue Shoes", "price": 45 }], "total": 2, "error": null }));
        let e = |x: ApiExpect| eval_expect(&x, &r);
        assert!(e(ApiExpect { status: Some(200), ..Default::default() }).0);
        assert!(!e(ApiExpect { status: Some(404), ..Default::default() }).0);
        assert!(e(ApiExpect { json: Some("results".into()), min_len: Some(2), ..Default::default() }).0);
        assert!(!e(ApiExpect { json: Some("results".into()), min_len: Some(3), ..Default::default() }).0);
        assert!(e(ApiExpect { json: Some("results.1.name".into()), contains: Some("blue".into()), ..Default::default() }).0, "case-insensitive contains on a string");
        assert!(e(ApiExpect { json: Some("results".into()), contains: Some("Red Shoes".into()), ..Default::default() }).0, "contains over an array looks inside its items");
        assert!(e(ApiExpect { json: Some("total".into()), equals: Some(json!(2)), ..Default::default() }).0);
        assert!(!e(ApiExpect { json: Some("total".into()), equals: Some(json!(3)), ..Default::default() }).0);
        assert!(e(ApiExpect { json: Some("results.0.sku".into()), exists: Some(false), ..Default::default() }).0);
        assert!(e(ApiExpect { contains: Some("blue shoes".into()), ..Default::default() }).0, "no path: the whole body");
        assert!(e(ApiExpect { status: Some(500), not: true, ..Default::default() }).0, "not inverts");
        let (ok, had) = e(ApiExpect { json: Some("results".into()), min_len: Some(9), ..Default::default() });
        assert!(!ok && had.contains("status 200") && had.contains("`results` = "), "{had}");
        // a body that is not JSON cannot satisfy a JSON check
        let text = Response { status: 200, body: "hello".into(), json: None, ms: 1 };
        assert!(!eval_expect(&ApiExpect { json: Some("a".into()), exists: Some(true), ..Default::default() }, &text).0);
        assert!(eval_expect(&ApiExpect { contains: Some("hello".into()), ..Default::default() }, &text).0);
    }

    #[test]
    fn a_server_change_is_told_from_a_screen_a_test_and_a_document() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(touches_backend(&v(&["src/server/search.ts"])) && touches_backend(&v(&["app/routes.py"])) && touches_backend(&v(&["serve/src/goals.rs"])));
        assert!(!touches_backend(&v(&["src/Search.tsx"])), "a screen is the browser's");
        assert!(!touches_backend(&v(&["src/server/search.ts", "src/App.css"])), "a change that also touches the UI is looked at in the browser");
        assert!(!touches_backend(&v(&["src/search.test.ts"])) && !touches_backend(&v(&["README.md"])) && !touches_backend(&v(&["vite.config.ts"])));
    }

    #[test]
    fn the_judge_is_told_what_the_server_answered_and_which_checks_failed() {
        let ev = Evidence { log: vec!["1. GET /api/search?q=shoes -> 200 in 4 ms".into()], checks: vec![("the status is 200".into(), true, "status 200".into()),
            ("`results` has at least 2 item(s)".into(), false, "status 200; `results` = []".into())], failed_steps: 1, ..Default::default() };
        let b = prompt_block(&ev);
        assert!(b.contains("1 check(s) passed, 1 failed") && b.contains("PASS the status is 200") && b.contains("FAIL `results` has at least 2 item(s) - the response had: status 200; `results` = []"), "{b}");
        assert!(api_plan_from_reply("Here you go: [{\"do\":\"get\",\"path\":\"/x\"},{\"do\":\"expect\",\"status\":200}] ok").is_some());
        assert!(api_plan_from_reply("no plan here").is_none());
        // the same tolerance as the UI plan: a bracket in a sentence first, a fence, a wrapping object
        let plan = "[{\"do\":\"get\",\"path\":\"/x\"},{\"do\":\"expect\",\"status\":200}]";
        assert!(api_plan_from_reply(&format!("see [1] and then:\n```json\n{plan}\n```")).is_some());
        assert!(api_plan_from_reply(&format!("{{\"steps\": {plan}}}")).is_some());
        assert!(api_plan_problem("only words").contains("no JSON array of requests"));
    }

    /// the runner against a real server (a tiny Node one if Node is here; the std library's otherwise): a search endpoint that works, one check that fails, and a server that is down
    #[test]
    fn a_plan_is_run_against_a_real_server_and_the_checks_decide() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten().take(6) {
                let mut s = stream; let _ = s.set_read_timeout(Some(Duration::from_millis(150)));
                // headers and body can arrive in separate segments: read until the client stops sending
                let mut all = vec![]; let mut buf = [0u8; 4096];
                loop { match s.read(&mut buf) { Ok(0) | Err(_) => break, Ok(n) => all.extend_from_slice(&buf[..n]) } }
                let req = String::from_utf8_lossy(&all).to_string(); let line = req.lines().next().unwrap_or("").to_string();
                let (status, body) = if line.starts_with("GET /api/search?q=shoes") { ("200 OK", r#"{"results":[{"name":"Red Shoes"},{"name":"Blue Shoes"}],"total":2}"#) }
                    else if line.starts_with("POST /api/cart") && req.contains("red-shoes") { ("201 Created", r#"{"items":[{"sku":"red-shoes","qty":1}]}"#) }
                    else { ("404 Not Found", r#"{"error":"not found"}"#) };
                let _ = write!(s, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            }
        });
        let base = format!("http://127.0.0.1:{port}");
        let plan = parse_api_plan(&json!([
            { "do": "get", "path": "/api/search?q=shoes" }, { "do": "expect", "status": 200, "json": "results", "min_len": 2 }, { "do": "expect", "json": "results.1.name", "contains": "blue" },
            { "do": "post", "path": "/api/cart", "body": { "sku": "red-shoes" } }, { "do": "expect", "status": 201, "json": "items.0.qty", "equals": 1 },
            { "do": "get", "path": "/api/search?q=zebra" }, { "do": "expect", "status": 200, "json": "results", "min_len": 1 }])).unwrap();
        let seen: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(vec![]));
        let live = Live { sink: { let seen = seen.clone(); Arc::new(move |v| seen.lock().unwrap().push(v)) }, pace: Arc::new(|| 0), film_dir: None, cancel: Default::default() };
        let ev = run_api_plan_live(&base, &plan, Path::new(""), Some(live)).unwrap();
        assert_eq!(ev.checks.len(), 4, "{:?}", ev.log);
        assert_eq!(ev.checks_failed(), 1, "only the zebra search fails: {:?}", ev.checks);
        assert!(!ev.checks[3].1 && ev.checks[3].2.contains("status 404"), "{:?}", ev.checks[3]);
        assert_eq!(ev.failed_steps, 1);
        let events = seen.lock().unwrap();
        let ex: Vec<&Value> = events.iter().filter(|e| e["type"] == "exchange").collect();
        assert_eq!(ex.len(), 3, "one exchange per request: {events:?}");
        assert_eq!((ex[0]["method"].as_str(), ex[0]["status"].as_u64()), (Some("GET"), Some(200)));
        assert!(ex[0]["body"].as_str().unwrap().contains("Red Shoes") && ex[1]["status"] == json!(201));
        assert_eq!(events.last().unwrap()["type"], "done");
        // a server that is not there is reported, and the checks that followed say there was nothing to check
        let down = run_api_plan("http://127.0.0.1:9", &parse_api_plan(&json!([{ "do": "get", "path": "/" }, { "do": "expect", "status": 200 }])).unwrap()).unwrap();
        assert_eq!(down.failed_steps, 2, "{:?}", down.log);
        assert!(down.log[1].contains("no response to check"), "{:?}", down.log);
    }
}
