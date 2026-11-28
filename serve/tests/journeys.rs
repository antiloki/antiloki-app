//! JOURNEYS (PLAN-THE-TESTS phase 2): whole paths through the REAL router with a SCRIPTED seat (tests/support/fake-claude standing in
//! for `claude`), on a throwaway substrate. Each test names the incident it locks. No network, no quota, no operator state.
#![cfg(unix)]

use antiloki_serve::{build_router, dispatch::AppState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

/// ONE environment for the whole binary: PATH gets the scripted `claude` first, chats live in a temp dir. Process-global on purpose
/// (the serve reads them from the environment) - so every journey runs against the same fake seat, and the seat's mode is chosen
/// per turn with a `FAKE:<mode>` marker in the text.
fn env_once() -> &'static PathBuf {
    static ONCE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("antiloki-journeys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin"); std::fs::create_dir_all(&bin).unwrap();
        let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude");
        std::os::unix::fs::symlink(&fake, bin.join("claude")).unwrap();
        // A PATH THAT REACHES ONLY SCRIPTED SEATS BY DEFAULT (2026-10-07): the developer's real `cursor-agent`, `codex`, `agy` and `grok` live under ~/.local and the mise installs, and since Heimdall now routes the QA
        // gate from the measured ranking (cli::qa_gate_ranking) - Claude on or off - an unpinned gate on a machine that HAS Cursor went to the real Cursor, spending its quota and failing seven journeys. The system
        // directories keep `git`, `python3` and Chrome; `ScriptedSeatsOnly` below is the same guard for the journeys that need a second scripted seat.
        let path = format!("{}:/usr/local/bin:/usr/bin:/bin", bin.display());
        std::env::set_var("PATH", path);
        std::env::set_var("ANTILOKI_CHATS_DIR", root.join("chats"));
        std::env::set_var("ANTILOKI_HOME", root.join("home"));
        std::env::set_var("ANTILOKI_NO_PATH_FALLBACK", "1");   // the PATH above is the whole of it
        std::env::set_var("ANTILOKI_BE_TICK_DISABLED", "true");
        std::env::set_var("ANTILOKI_THEMES_DIR", root.join("themes"));   // antiloki's own theme is written into the store on first listing: never into a developer's real one
        std::env::set_var("ANTILOKI_SHIPPED_BENCH", "off");   // the routing journeys assume a record they built themselves, not the benchmarks antiloki ships
        // the scripted `claude` is THE seat of these journeys, so it must read as signed in whatever the developer's (or CI's) HOME holds: `engine_seat` counts an API key as a seat
        std::env::set_var("ANTHROPIC_API_KEY", "scripted");
        // THE DESIGN ROUND (S5) is OFF for every journey but J18's, and its critic - if one ever runs - is the scripted claude: a journey must never reach a real seat
        std::env::set_var("ANTILOKI_QA_DESIGN", "off"); std::env::set_var("ANTILOKI_DESIGN_CRITIC", "claude-code:sonnet");
        extern "C" fn cleanup() { let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("antiloki-journeys-{}", std::process::id()))); }
        extern "C" { #[link_name = "atexit"] fn atexit(f: extern "C" fn()) -> i32; }
        unsafe { atexit(cleanup); }
        root
    })
}

/// Journeys run ONE AT A TIME: each has its own substrate, so pane ids repeat across them (every chat is pane 1), and the chats directory
/// (where a pane's event log lives) is process-global - two journeys at once would write the same log.
fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static L: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    loop { if let Ok(g) = L.try_lock() { return g; } std::thread::sleep(std::time::Duration::from_millis(20)); }
}

fn app() -> axum::Router { build_router(fresh_state(0)) }

/// A throwaway state with `pool` extra connections (J9c needs several; the router tests run on the primary alone).
fn fresh_state(pool: usize) -> Arc<AppState> {
    let root = env_once();
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = root.join(format!("sub-{}", N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    std::env::set_var("ANTILOKI_CHATS_DIR", dir.join("chats"));
    // the run history, queue, ledger and merge record live under ANTILOKI_HOME: one per journey, or an earlier journey's asks leak into the next one's prompts
    std::env::set_var("ANTILOKI_HOME", dir.join("home"));
    // ...AND THE DATA DIR + SUBSTRATE ROOT (found 2026-09-30): without them the workspace registry (`workspaces/add|activate`), sources and the sandbox resolve to the
    // developer substrate next to the repo (<repo>/../.antiloki), so J14 registered - and ACTIVATED - a workspace there. A journey must never write outside its own dir.
    std::env::set_var("ANTILOKI_DATA_DIR", dir.join("data"));
    std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", dir.join("substrate"));
    std::fs::create_dir_all(dir.join("data").join("db")).unwrap();
    // the app's own state DB IS the sandboxed data dir's registry (workspaces/add opens <data dir>/db/state.sqlite itself), so seed that file
    let db_dir = dir.join("data").join("db"); std::fs::create_dir_all(&db_dir).unwrap();
    let path = json!(db_dir.join("state.sqlite").to_string_lossy());
    let open = || antiloki_rs::engine_open_db::openDb(&path, &json!({ "readOnly": false })).expect("openDb seeds a fresh substrate");
    let conn = open();
    Arc::new(AppState {
        conn: Arc::new(Mutex::new(conn)), pool: (0..pool).map(|_| Arc::new(Mutex::new(open()))).collect(), events: antiloki_serve::events::EventsHub::new(), waker: std::sync::OnceLock::new(),
        // the router journeys keep their ":memory:" (a workspace switch there rebinds to a throwaway); a pooled state rebinds onto the real file, which is what J9c watches
        db_path: if pool == 0 { ":memory:".into() } else { db_dir.join("state.sqlite").to_string_lossy().into_owned() },
    })
}

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder().method(method).uri(uri).header("content-type", "application/json")
        .body(body.map(|b| Body::from(b.to_string())).unwrap_or(Body::empty())).unwrap();
    let res = app.clone().oneshot(req).await.expect("router responds");
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// the pane's event log, as the stream replays it: one JSON object per line (`{seq, t, ev}`)
fn events_of(worktree: &str, pane_id: i64) -> Vec<Value> {
    let p = antiloki_serve::agent::events_read_path(worktree, pane_id);
    std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect()
}

/// wait until `pred` holds over the pane's events (the seat is a real subprocess - the answer arrives when it arrives)
async fn until(worktree: &str, pane_id: i64, what: &str, pred: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    for _ in 0..200 {
        let ev = events_of(worktree, pane_id);
        if pred(&ev) { return ev; }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}\nevents so far:\n{}", events_of(worktree, pane_id).iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"));
}

async fn new_chat(app: &axum::Router) -> (i64, String) {
    let (st, v) = call(app, "POST", "/api/v1/cockpit/chat/create", Some(json!({ "engine": "claude-code", "title": "journey" }))).await;
    assert_eq!(st, StatusCode::CREATED, "chat/create: {v}");
    let pane = v["pane"]["id"].as_i64().expect("pane id");
    // QA IS OPTIONAL (2026-10-10): a new pane has it off, so the journeys that exercise QA switch it on the way a user does - in the pane's settings - and keep exactly the behaviour they were written for
    let mut settings: Value = serde_json::from_str(v["pane"]["settings_json"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
    settings["qa"] = json!({ "enabled": true });
    let (st2, u) = call(app, "POST", "/api/v1/cockpit/pane/update", Some(json!({ "pane_id": pane, "settings": settings }))).await;
    assert!(st2.is_success(), "pane/update (qa on): {u}");
    (pane, v["pane"]["worktree_path"].as_str().expect("worktree").to_string())
}

fn sub(ev: &Value) -> String { ev["ev"]["subtype"].as_str().unwrap_or("").to_string() }
fn ty(ev: &Value) -> String { ev["ev"]["type"].as_str().unwrap_or("").to_string() }
fn has_reply(ev: &[Value], text: &str) -> bool { ev.iter().any(|e| ty(e) == "assistant" && e["ev"].to_string().contains(text)) }

// J1 - INCIDENT: "the vanished message" / "joiner got no replay". A message sent to a chat comes back as the seat's reply in the SAME
// event log every stream replays from, in order, with a monotonically increasing seq and no duplicates.
#[tokio::test(flavor = "multi_thread")]
async fn j1_a_message_comes_back_as_a_reply_in_the_replayable_log() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await;
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": "hello journey" }))).await;
    assert!(st.is_success(), "agent/send: {st} {v}");
    let ev = until(&wt, pane, "the reply", |e| has_reply(e, "hello journey") && e.iter().any(|x| ty(x) == "result")).await;
    let seqs: Vec<i64> = ev.iter().filter_map(|e| e["seq"].as_i64()).collect();
    assert!(seqs.windows(2).all(|w| w[1] > w[0]), "seq must strictly increase: {seqs:?}");
    let users = ev.iter().filter(|e| sub(e) == "user" || (ty(e) == "user")).count();
    assert!(users <= 1, "the operator's message must be logged at most once, saw {users}");
    let replies = ev.iter().filter(|e| ty(e) == "assistant" && e["ev"].to_string().contains("hello journey")).count();
    assert_eq!(replies, 1, "one reply, not {replies}");
    // and the transcript endpoint (what a joining tab reads) tells the same story
    let (st, t) = call(&app, "GET", &format!("/api/v1/cockpit/agent/transcript?pane_id={pane}"), None).await;
    assert!(st.is_success() && t["transcript"].as_str().unwrap_or("").contains("hello journey"), "transcript: {t}");
}

// J1b - INCIDENT: a seat that fails must say so in the log, and the pane must still take the next message.
#[tokio::test(flavor = "multi_thread")]
async fn j1b_a_failing_turn_is_reported_and_the_next_message_still_works() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await;
    call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": "FAKE:error break it" }))).await;
    let ev = until(&wt, pane, "the error result", |e| e.iter().any(|x| ty(x) == "result" && x["ev"]["is_error"] == json!(true))).await;
    assert!(ev.iter().any(|e| ty(e) == "result" && e["ev"].to_string().contains("failed on purpose")), "the seat's error text must reach the log");
    call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": "second try" }))).await;
    until(&wt, pane, "the next reply", |e| has_reply(e, "second try")).await;
}

// J1c - INCIDENT: ordering. The operator's words must be in the log BEFORE the seat's answer to them, for every turn - a reply that lands
// above its question is how a thread reads as "the message vanished" or the answer belongs to the wrong ask.
#[tokio::test(flavor = "multi_thread")]
async fn j1c_every_operator_message_is_logged_before_its_reply() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await;
    for n in 1..=4 {
        let word = format!("order-probe-{n}");
        let (st, v) = call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": word }))).await;
        assert!(st.is_success(), "send {n}: {st} {v}");
        until(&wt, pane, &format!("reply {n}"), |e| e.iter().filter(|x| ty(x) == "result").count() >= n).await;
    }
    let ev = events_of(&wt, pane);
    for n in 1..=4 {
        let word = format!("order-probe-{n}");
        let user = ev.iter().position(|e| sub(e) == "user" && e["ev"]["text"].as_str().map_or(false, |t| t.contains(&word)));
        let reply = ev.iter().position(|e| ty(e) == "assistant" && e["ev"].to_string().contains(&word));
        let (Some(u), Some(r)) = (user, reply) else { panic!("turn {n}: user event {user:?}, reply {reply:?}") };
        assert!(u < r, "turn {n}: the operator's message is logged at position {u}, AFTER the reply at {r}");
    }
}

fn git(dir: &str, args: &[&str]) -> String {
    let o = std::process::Command::new("git").args(["-c", "user.email=j@example.com", "-c", "user.name=j"]).args(args).current_dir(dir).output().expect("git runs");
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn note_with(ev: &[Value], needle: &str) -> bool { ev.iter().any(|e| sub(e) == "note" && e["ev"]["text"].as_str().map_or(false, |t| t.contains(needle))) }

// J2 - INCIDENT: two messages sent back to back (the second while the first is still running). Both are logged as they are typed, each reply
// answers its own ask, and the replies come back in the order the asks went in.
#[tokio::test(flavor = "multi_thread")]
async fn j2_two_messages_in_a_row_are_answered_in_order() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await;
    call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": "FAKE:slow:700 first-ask" }))).await;
    call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": "second-ask" }))).await;
    let ev = until(&wt, pane, "both replies", |e| has_reply(e, "first-ask") && has_reply(e, "second-ask")).await;
    let pos = |pred: &dyn Fn(&Value) -> bool| ev.iter().position(|e| pred(e)).expect("event present");
    let u1 = pos(&|e| sub(e) == "user" && e["ev"]["text"].as_str().map_or(false, |t| t.contains("first-ask")));
    let u2 = pos(&|e| sub(e) == "user" && e["ev"]["text"].as_str().map_or(false, |t| t.contains("second-ask")));
    let r1 = pos(&|e| ty(e) == "assistant" && e["ev"].to_string().contains("first-ask"));
    let r2 = pos(&|e| ty(e) == "assistant" && e["ev"].to_string().contains("second-ask"));
    assert!(u1 < r1 && u2 < r2, "each ask is logged before its own reply: u1={u1} r1={r1} u2={u2} r2={r2}");
    assert!(r1 < r2, "replies come back in the order the asks went in: r1={r1} r2={r2}");
}

// J8 - INCIDENT: "make a single checkpoint every response like a git history" (operator 2026-09-29). A response that changes the box gets ONE
// checkpoint, in git objects under refs/antiloki/cp/pane-N/n, announced in the thread; a response that changes nothing gets none.
#[tokio::test(flavor = "multi_thread")]
async fn j8_a_response_that_edits_the_box_gets_one_checkpoint_and_one_that_does_not_gets_none() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await;
    git(&wt, &["init", "-q"]); std::fs::write(std::path::Path::new(&wt).join("seed.txt"), "seed\n").unwrap(); git(&wt, &["add", "-A"]); git(&wt, &["commit", "-q", "-m", "seed"]);
    call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": "FAKE:write:notes/a.txt write a note" }))).await;
    let ev = until(&wt, pane, "a checkpoint note", |e| note_with(e, "checkpoint")).await;
    assert!(std::path::Path::new(&wt).join("notes/a.txt").exists(), "the seat's edit is in the box");
    let refs = git(&wt, &["for-each-ref", "refs/antiloki/cp/"]);
    assert!(refs.contains(&format!("pane-{pane}/")), "the checkpoint is a git ref for this pane: {refs:?}");
    let before = refs.lines().count();
    // a second response that changes nothing adds no checkpoint
    call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": "just talk, touch nothing" }))).await;
    until(&wt, pane, "the second reply", |e| has_reply(e, "just talk, touch nothing")).await;
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;   // the checkpoint is taken off the request path
    let after = git(&wt, &["for-each-ref", "refs/antiloki/cp/"]).lines().count();
    assert_eq!(after, before, "an unchanged box adds no checkpoint ({before} -> {after})");
    assert_eq!(ev.iter().filter(|e| sub(e) == "note" && e["ev"]["text"].as_str().map_or(false, |t| t.contains("▸ checkpoint"))).count(), 1, "announced once");
}

// J4 - INCIDENT: "you made a question, I said yes, and you forgot the theme of the conversation" (operator 2026-09-29). What the chat proposed
// is read back from the chat's own log by code: a bare yes with ONE proposal accepts it; with several it asks which (and says so in the
// thread); then the number picks. No model is asked in any of it.
#[tokio::test(flavor = "multi_thread")]
async fn j4_a_yes_after_a_proposal_accepts_it_and_after_several_asks_which() {
    let _s = serial(); let app = app();
    // one proposal
    let (pane, wt) = new_chat(&app).await;
    call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": "\n▸ proposal: add a docs page for the queue" }))).await;
    until(&wt, pane, "the reply carrying the proposal", |e| has_reply(e, "▸ proposal: add a docs page for the queue")).await;
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/agent/front", Some(json!({ "pane_id": pane, "text": "yes" }))).await;
    assert!(st.is_success(), "front: {st} {v}");
    assert_eq!(v["handled"], json!(false), "the ask goes on to the plan door as a task: {v}");
    assert_eq!(v["kind"], json!("task"), "{v}");
    assert!(v["ask"].as_str().unwrap_or("").contains("add a docs page for the queue"), "the accepted proposal is the ask: {v}");
    // several proposals
    let (pane2, wt2) = new_chat(&app).await;
    call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane2, "text": "\n▸ proposal: add a docs page\n▸ proposal: rename the module" }))).await;
    until(&wt2, pane2, "the reply carrying both proposals", |e| has_reply(e, "▸ proposal: rename the module")).await;
    let (_, v) = call(&app, "POST", "/api/v1/cockpit/agent/front", Some(json!({ "pane_id": pane2, "text": "yes" }))).await;
    assert_eq!(v["handled"], json!(true), "a bare yes with two proposals is answered here, with a question: {v}");
    assert_eq!(v["kind"], json!("ask"), "{v}");
    let ev = events_of(&wt2, pane2);
    assert!(note_with(&ev, "which one?"), "the thread says it needs a number");
    let (_, v) = call(&app, "POST", "/api/v1/cockpit/agent/front", Some(json!({ "pane_id": pane2, "text": "2" }))).await;
    assert_eq!(v["kind"], json!("task"), "{v}");
    assert!(v["ask"].as_str().unwrap_or("").contains("rename the module"), "the number picks the second proposal: {v}");
}

/// a git box for a goal run: the chat's own folder, one commit in it
fn git_box(wt: &str) { git(wt, &["init", "-q"]); std::fs::write(std::path::Path::new(wt).join("seed.txt"), "seed\n").unwrap(); git(wt, &["add", "-A"]); git(wt, &["commit", "-q", "-m", "seed"]); }

async fn queue_add(app: &axum::Router, pane: i64, wt: &str, goal: &str) -> Value {
    let (st, v) = call(app, "POST", "/api/v1/cockpit/queue/add", Some(json!({ "parent": format!("1-{pane}"), "goal": goal, "engine": "claude-code", "cwd": wt, "write": true, "n": 1, "pane_id": pane, "always": true }))).await;
    assert!(st.is_success(), "queue/add: {st} {v}");
    v
}
/// runs live in a store shared by every journey (and every chat is pane 1): a journey's runs are the ones whose goal carries its TAG
async fn runs_of(app: &axum::Router, pane: i64, tag: &str) -> Vec<Value> {
    let (_, v) = call(app, "GET", "/api/v1/cockpit/goals", None).await;
    v["runs"].as_array().cloned().unwrap_or_default().into_iter().filter(|r| r["pane_id"].as_i64() == Some(pane) && r["goal"].as_str().map_or(false, |g| g.contains(tag))).collect()
}
async fn run_until(app: &axum::Router, pane: i64, tag: &str, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
    let mut last = Value::Null;
    for _ in 0..600 {
        if let Some(r) = runs_of(app, pane, tag).await.into_iter().last() { last = r.clone(); if pred(&r) { return r; } }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}\nlast run: {}", serde_json::to_string_pretty(&last).unwrap_or_default());
}

// J5 - the pipeline, happy path: an ask through the queue is screened, planned, ratified, built by a worker, verified, joined and passed by
// the QA gate; the file is in the box; the queue item is done; the box has a checkpoint from before and after.
#[tokio::test(flavor = "multi_thread")]
async fn j5_an_ask_goes_through_the_whole_pipeline_and_lands_in_the_box() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j5"));
    let added = queue_add(&app, pane, &wt, "create the greeting file FAKE:write:out/hello.txt tag-j5").await;
    assert!(added["item"]["id"].is_number(), "queue item: {added}");
    let run = run_until(&app, pane, "tag-j5", "the run to finish", |r| matches!(r["status"].as_str(), Some("done" | "failed" | "cancelled" | "error"))).await;
    assert_eq!(run["status"], json!("done"), "the run: {}", serde_json::to_string_pretty(&run).unwrap_or_default());
    assert!(std::path::Path::new(&wt).join("out/hello.txt").exists(), "the worker's file is in the box; run: {}", serde_json::to_string_pretty(&run).unwrap_or_default());
    // the queue settles an item on its next tick (1.5 s), so wait for it rather than sampling once
    let mut q = Value::Null; let mut item = Value::Null;
    for _ in 0..100 {
        q = call(&app, "GET", &format!("/api/v1/cockpit/queue?pane_id={pane}"), None).await.1;
        item = q["items"].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        if item["status"] == json!("done") { break; }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(item["status"], json!("done"), "the queue item follows the run: {q}");
    assert!(item["cp_before"].is_object() || !q["checkpoints"].as_array().map_or(true, |c| c.is_empty()), "a checkpoint was taken around the item: {q}");
}

// J6a - INCIDENT: run #259 ("why didn't the QA send it back?"). QA finds the work unmet -> the run is SENT BACK to a worker with QA's words,
// it is done again, QA passes: the run ends done, after exactly one send-back.
#[tokio::test(flavor = "multi_thread")]
async fn j6a_qa_sends_unmet_work_back_and_the_second_pass_delivers() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j6a"));
    queue_add(&app, pane, &wt, "create the note FAKE:write:out/note.txt FAKE:qa:1 tag-j6a").await;
    let run = run_until(&app, pane, "tag-j6a", "the run to settle after the send-back", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false) && r["qa_rounds"].as_i64().unwrap_or(0) >= 1).await;
    assert_eq!(run["qa_rounds"], json!(1), "one send-back: {}", serde_json::to_string_pretty(&run["events"]).unwrap_or_default());
    let kinds: Vec<String> = run["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap_or("").to_string()).collect();
    assert!(kinds.iter().any(|k| k == "qa-fail" || k == "qa-back" || k.starts_with("qa")), "the log says QA acted: {kinds:?}");
    assert!(std::path::Path::new(&wt).join("out/note.txt").exists());
}

// J6b - "if the QA send back 2 times and on the third is still wrong, this becomes a screening question explaining what happened and asking for
// new direction" (operator 2026-09-29) - never seen live before this journey.
#[tokio::test(flavor = "multi_thread")]
async fn j6b_after_two_send_backs_qa_asks_the_operator_for_a_direction() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j6b"));
    queue_add(&app, pane, &wt, "create the note FAKE:write:out/note.txt FAKE:qa:9 tag-j6b").await;
    let run = run_until(&app, pane, "tag-j6b", "the run to park on QA's question", |r| r["status"] == json!("awaiting-answers") || r["status"] == json!("awaiting_answers") || r["status"] == json!("AwaitingAnswers")).await;
    assert_eq!(run["qa_rounds"], json!(2), "two send-backs before the question: {}", serde_json::to_string_pretty(&run["events"]).unwrap_or_default());
    let q = &run["questions"][0];
    assert_eq!(q["kind"], json!("qa-direction"), "the question is QA's: {q}");
    assert!(q["text"].as_str().unwrap_or("").contains("sent this run back 2 time"), "it says what happened: {q}");
    assert!(q["options"].as_array().map_or(false, |o| o.len() >= 2), "and offers directions: {q}");
}

async fn answer_qa(app: &axum::Router, run_id: i64, choice: &str) -> Value {
    let (st, v) = call(app, "POST", "/api/v1/cockpit/goals/answer", Some(json!({ "id": run_id, "answers": { "qa1": choice } }))).await;
    assert!(st.is_success(), "goals/answer: {st} {v}");
    v
}
fn awaiting(r: &Value) -> bool { r["status"].as_str().map_or(false, |s| s.replace(['-', '_'], "").to_lowercase() == "awaitinganswers") }

// J6c - the operator answers QA's question with "accept it as it is": the run is done, delivered with QA's unmet note written on it.
#[tokio::test(flavor = "multi_thread")]
async fn j6c_accepting_the_run_as_it_is_settles_it_with_qas_note() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j6c"));
    queue_add(&app, pane, &wt, "create the note FAKE:write:out/note.txt FAKE:qa:9 tag-j6c").await;
    let run = run_until(&app, pane, "tag-j6c", "QA's question", awaiting).await;
    answer_qa(&app, run["id"].as_i64().unwrap(), "accept it as it is").await;
    let done = run_until(&app, pane, "tag-j6c", "the run to be done", |r| r["status"] == json!("done")).await;
    assert!(done["report"].as_str().unwrap_or("").contains("accepted by the operator"), "QA's verdict stays on the report: {}", done["report"]);
}

// J6d - "keep trying with the same feedback": the workers get another go with QA's words; when QA is finally satisfied the run is done.
#[tokio::test(flavor = "multi_thread")]
async fn j6d_keep_trying_sends_the_run_back_again_and_a_passing_check_delivers() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j6d"));
    queue_add(&app, pane, &wt, "create the note FAKE:write:out/note.txt FAKE:qa:3 tag-j6d").await;
    let run = run_until(&app, pane, "tag-j6d", "QA's question", awaiting).await;
    answer_qa(&app, run["id"].as_i64().unwrap(), "keep trying with the same feedback").await;
    let done = run_until(&app, pane, "tag-j6d", "the run to be done after the operator's direction", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false)).await;
    let kinds: Vec<&str> = done["events"].as_array().unwrap().iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(kinds.contains(&"qa-send-back"), "the log shows the operator's direction sent it back: {kinds:?}");
    assert!(!done["report"].as_str().unwrap_or("").contains("accepted by the operator"), "this one was earned, not accepted");
}

// J7 - screening: an ask that the screening step finds under-specified parks the run with a question (never a guess), one answer round, and
// the answer sends it on to be built.
#[tokio::test(flavor = "multi_thread")]
async fn j7_a_screened_ask_parks_on_its_question_and_the_answer_lets_it_build() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j7"));
    queue_add(&app, pane, &wt, "create the summary file FAKE:ask FAKE:write:out/summary.txt tag-j7").await;
    let run = run_until(&app, pane, "tag-j7", "the screening question", awaiting).await;
    let q = &run["questions"][0];
    assert!(q["text"].as_str().unwrap_or("").contains("Which format"), "the question is the screening step's: {q}");
    assert!(!std::path::Path::new(&wt).join("out/summary.txt").exists(), "nothing was built on a guess");
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/goals/answer", Some(json!({ "id": run["id"], "answers": { q["id"].as_str().unwrap(): "plain text" } }))).await;
    assert!(st.is_success(), "goals/answer: {st} {v}");
    run_until(&app, pane, "tag-j7", "the run to be done after the answer", |r| r["status"] == json!("done")).await;
    assert!(std::path::Path::new(&wt).join("out/summary.txt").exists(), "the answer let it build");
}

async fn queue_items(app: &axum::Router, pane: i64) -> Vec<Value> {
    call(app, "GET", &format!("/api/v1/cockpit/queue?pane_id={pane}"), None).await.1["items"].as_array().cloned().unwrap_or_default()
}
fn item_id(v: &Value) -> i64 { v["item"]["id"].as_i64().expect("queue item id") }
fn status_of(items: &[Value], id: i64) -> String { items.iter().find(|i| i["id"].as_i64() == Some(id)).and_then(|i| i["status"].as_str()).unwrap_or("").to_string() }

// J3 - the queue is a todo list the operator can rearrange: with one item doing, later items wait in order; they can be re-ordered,
// re-worded (and re-bound) and cancelled; the survivors then run in the new order.
#[tokio::test(flavor = "multi_thread")]
async fn j3_queued_items_can_be_reordered_reprompted_and_cancelled() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j3"));
    let a = item_id(&queue_add(&app, pane, &wt, "create the first file FAKE:slow:2500 FAKE:write:out/a.txt tag-j3a").await);
    let b = item_id(&queue_add(&app, pane, &wt, "create the second file FAKE:write:out/b.txt tag-j3b").await);
    let c = item_id(&queue_add(&app, pane, &wt, "create the third file FAKE:write:out/c.txt tag-j3c").await);
    let items = queue_items(&app, pane).await;
    assert_eq!((status_of(&items, a).as_str(), status_of(&items, b).as_str(), status_of(&items, c).as_str()), ("doing", "queued", "queued"), "one at a time: {items:?}");
    // C jumps ahead of B
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/queue/reorder", Some(json!({ "id": c, "index": 0 }))).await; assert!(st.is_success(), "reorder: {st} {v}");
    let queued: Vec<i64> = queue_items(&app, pane).await.iter().filter(|i| i["status"] == json!("queued")).filter_map(|i| i["id"].as_i64()).collect();
    assert_eq!(queued, vec![c, b], "C is next now");
    // B is re-worded; the doing item cannot be reordered, and cancelling C removes it from the line
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/queue/reprompt", Some(json!({ "id": b, "goal": "create the second file, reworded FAKE:write:out/b.txt tag-j3b2" }))).await; assert!(st.is_success(), "reprompt: {st} {v}");
    let (st, _) = call(&app, "POST", "/api/v1/cockpit/queue/reorder", Some(json!({ "id": a, "index": 1 }))).await; assert!(!st.is_success(), "an item that is doing cannot be moved");
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/queue/cancel", Some(json!({ "id": c }))).await; assert!(st.is_success(), "cancel: {st} {v}");
    assert_eq!(status_of(&queue_items(&app, pane).await, c), "cancelled");
    // A finishes, then B (as reworded) runs; C never does
    run_until(&app, pane, "tag-j3a", "A to be done", |r| r["status"] == json!("done")).await;
    let rb = run_until(&app, pane, "tag-j3b2", "the reworded B to be done", |r| r["status"] == json!("done")).await;
    assert!(rb["goal"].as_str().unwrap().contains("reworded"), "B ran with its new words");
    assert!(runs_of(&app, pane, "tag-j3c").await.is_empty(), "the cancelled item never became a run");
    assert!(std::path::Path::new(&wt).join("out/b.txt").exists() && !std::path::Path::new(&wt).join("out/c.txt").exists());
}

// J7b - the DETERMINISTIC screen (no model called): an ask that names a file that is not in the box, without saying to create it, parks with a
// `target` question whose evidence is code, and its chips include "create it".
#[tokio::test(flavor = "multi_thread")]
async fn j7b_an_ask_naming_a_missing_file_is_stopped_by_the_deterministic_screen() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j7b"));
    queue_add(&app, pane, &wt, "update src/missing_module.rs to log the error tag-j7b").await;
    let run = run_until(&app, pane, "tag-j7b", "the screening question", awaiting).await;
    let q = &run["questions"][0];
    assert_eq!(q["kind"], json!("target"), "{q}");
    assert!(q["text"].as_str().unwrap_or("").contains("src/missing_module.rs"), "the question names the file: {q}");
    assert!(q["options"].as_array().unwrap().iter().any(|o| o.as_str().map_or(false, |o| o.starts_with("create"))), "and offers to create it: {q}");
    let events: Vec<String> = run["events"].as_array().unwrap().iter().map(|e| format!("{} {}", e["kind"].as_str().unwrap_or(""), e["text"].as_str().unwrap_or(""))).collect();
    assert!(events.iter().any(|e| e.contains("no model was called")), "the log says code found it, not a model: {events:?}");
}

// J14 - INCIDENT: onboarding's "open folder" was a dead end on Linux ("no native folder dialog on this machine - type the path instead").
// The browser tab walks the serve's folder listing: directories only, sorted, no dotfolders, repos flagged, a way back up, and an honest
// error for a missing path or a file - never a panic and never an empty success.
#[tokio::test(flavor = "multi_thread")]
async fn j14_the_folder_browser_lists_projects_and_explains_a_bad_path() {
    let _s = serial(); let app = app();
    let root = env_once().join("browse-root");
    let _ = std::fs::remove_dir_all(&root);
    for d in ["alpha/.git", "Beta", ".hidden", "gamma/deeper"] { std::fs::create_dir_all(root.join(d)).unwrap(); }
    std::fs::write(root.join("file.txt"), "x").unwrap();
    std::os::unix::fs::symlink(root.join("Beta"), root.join("link-to-beta")).unwrap();
    let r = root.to_string_lossy().to_string();
    let (st, v) = call(&app, "GET", &format!("/api/v1/workspaces/browse?path={r}"), None).await;
    assert!(st.is_success() && v["ok"] == json!(true), "{v}");
    let names: Vec<&str> = v["dirs"].as_array().unwrap().iter().filter_map(|d| d["name"].as_str()).collect();
    assert_eq!(names, vec!["alpha", "Beta", "gamma"], "directories only, case-insensitive order, no dotfolders, no files: {names:?}");
    assert_eq!(v["dirs"][0]["repo"], json!(true), "a git repo is flagged"); assert_eq!(v["dirs"][1]["repo"], json!(false));
    assert_eq!(v["parent"], json!(root.parent().unwrap().to_string_lossy()), "and the way back up");
    // one level down, then a bad path, then a file
    let (_, v) = call(&app, "GET", &format!("/api/v1/workspaces/browse?path={r}/gamma"), None).await;
    assert_eq!(v["dirs"][0]["name"], json!("deeper"));
    let (st, v) = call(&app, "GET", &format!("/api/v1/workspaces/browse?path={r}/nope"), None).await;
    assert!(st.is_success() && v["ok"] == json!(false) && v["error"].as_str().map_or(false, |e| !e.is_empty()), "a missing folder says so: {v}");
    let (_, v) = call(&app, "GET", &format!("/api/v1/workspaces/browse?path={r}/file.txt"), None).await;
    assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap_or("").contains("file, not a folder"), "{v}");
    // and the folder it found can become the workspace
    let (st, v) = call(&app, "POST", "/api/v1/workspaces/add", Some(json!({ "name": "browsed", "root_path": format!("{r}/alpha") }))).await;
    assert!(st.is_success() && v["ok"] == json!(true), "workspaces/add: {v}");
    let (_, v) = call(&app, "POST", "/api/v1/workspaces/activate", Some(json!({ "name": "browsed" }))).await;
    assert_eq!(v["active"], json!("browsed"), "{v}");
}

// J15 - the operator's agreed item 7 (2026-09-30): a run the QA gate PASSED and the operator then UNDID is a false accept - the one error nothing else
// catches. The undo says so in the thread and counts it against the judge that accepted the run.
#[tokio::test(flavor = "multi_thread")]
async fn j15_undoing_a_run_the_qa_gate_passed_is_counted_as_a_false_accept() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j15"));
    // the chat's agent must be running for the thread to keep its notes (events are persisted once the pane has a box)
    call(&app, "POST", "/api/v1/cockpit/agent/send", Some(json!({ "pane_id": pane, "text": "warm up" }))).await;
    until(&wt, pane, "the warm-up reply", |e| has_reply(e, "warm up")).await;
    let added = queue_add(&app, pane, &wt, "create the accepted file FAKE:write:out/accepted.txt tag-j15").await;
    let item = item_id(&added);
    let run = run_until(&app, pane, "tag-j15", "the run to be done and reviewed", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false)).await;
    // the premise of a false accept: QA accepted it - the judge passed it, or the claim gate proved a claim and found none unmet
    let accepted = run["qa_review"]["passed"] == json!(true) || (run["qa_gate"]["met"].as_i64().unwrap_or(0) > 0 && run["qa_gate"]["unmet"].as_i64().unwrap_or(1) == 0);
    assert!(accepted, "QA accepted the run: review {} gate {}", run["qa_review"], run["qa_gate"]);
    for _ in 0..100 { if status_of(&queue_items(&app, pane).await, item) == "done" { break } tokio::time::sleep(std::time::Duration::from_millis(100)).await; }
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/queue/restore", Some(json!({ "id": item }))).await;
    assert!(st.is_success(), "restore: {st} {v}");
    let ev = events_of(&wt, pane);
    let table = call(&app, "GET", "/api/v1/settings/merges", None).await.1;
    let judges: Vec<String> = table["rows"].as_array().map(|a| a.iter().filter(|r| r["job"] == "qa-gate").map(|r| format!("{}|{}", r["planner"], r["size"])).collect()).unwrap_or_default();
    assert!(note_with(&ev, "false accept"), "qa-gate rows in this journey's record: {judges:?}; the thread says the undo was counted against the judge: {:?}", ev.iter().filter(|e| sub(e) == "note").map(|e| e["ev"]["text"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>());
}

// J16 - VISUAL QA (operator 2026-09-30): a change to something you can SEE is judged on what it looks like. The QA chooses the steps, deterministic tools capture them in a
// real browser, and the claim checks are handed the images. Needs the machine's Chrome (skips without one).
#[tokio::test(flavor = "multi_thread")]
async fn j16_a_ui_change_is_shown_to_the_qa_as_screenshots_a_deterministic_tool_took() {
    if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome here - the visual QA journey is skipped"); return }
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    let state = std::path::Path::new(&wt).join("../fake-state-j16"); std::env::set_var("FAKE_SEAT_STATE_DIR", &state);
    queue_add(&app, pane, &wt, "create the landing page FAKE:write:index.html tag-j16").await;
    let run = run_until(&app, pane, "tag-j16", "the run to be done", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false)).await;
    let events: Vec<String> = run["events"].as_array().unwrap().iter().map(|e| format!("{}: {}", e["kind"].as_str().unwrap_or(""), e["text"].as_str().unwrap_or(""))).collect();
    let line = events.iter().find(|e| e.starts_with("qa-evidence")).unwrap_or_else(|| panic!("the run logged its visual evidence: {events:?}"));
    assert!(line.contains("1 screenshot(s)") && line.contains("the QA chose the steps"), "{line}");
    let shot = std::path::Path::new(&wt).join(".antiloki/qa/evidence").join(format!("run-{}", run["id"])).join("01-page.png");
    let bytes = std::fs::read(&shot).unwrap_or_else(|e| panic!("the screenshot {} exists: {e}", shot.display()));
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "a real PNG");
    let claim = std::fs::read_to_string(state.join("claim-prompt.txt")).expect("a claim check ran");
    assert!(claim.contains("VISUAL EVIDENCE") && claim.contains("IMAGE page:") && claim.contains("01-page.png"), "the claim check was handed the image: {claim}");
    // the UI's door: the list, the bytes, and no way out of the folder
    let id = run["id"].as_i64().unwrap();
    let (st, list) = call(&app, "GET", &format!("/api/v1/cockpit/goals/evidence?id={id}"), None).await;
    assert!(st.is_success() && list["images"].as_array().map_or(false, |a| a.iter().any(|i| i["file"] == "01-page.png" && i["side"] == "after" && i["name"] == "page")), "{list}");
    let res = axum::Router::clone(&app).oneshot(Request::builder().method("GET").uri(format!("/api/v1/cockpit/goals/evidence?id={id}&file=01-page.png")).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK); assert_eq!(res.headers()["content-type"], "image/png");
    assert_eq!(&res.into_body().collect().await.unwrap().to_bytes()[..8], b"\x89PNG\r\n\x1a\n");
    let (st, _) = call(&app, "GET", &format!("/api/v1/cockpit/goals/evidence?id={id}&file=..%2F..%2Fseed.txt"), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a path is not an image name");
}

// J16b - a change with nothing to look at (a backend file) takes no screenshots and does not pretend to.
#[tokio::test(flavor = "multi_thread")]
async fn j16b_a_backend_change_takes_no_screenshots() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j16b"));
    queue_add(&app, pane, &wt, "create the module FAKE:write:src/server.rs tag-j16b").await;
    let run = run_until(&app, pane, "tag-j16b", "the run to be done", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false)).await;
    // (a server change may now log that there is nothing to CALL - but never a screenshot, because there is nothing to look at)
    assert!(run["events"].as_array().unwrap().iter().all(|e| e["kind"] != "qa-evidence" || (!e["text"].as_str().unwrap_or("").contains("screenshot") && e["text"].as_str().unwrap_or("").contains("nothing to call"))), "{:?}", run["events"]);
    assert!(!std::path::Path::new(&wt).join(".antiloki/qa/evidence").exists());
}

// J16e - A PLAN THAT TESTS (operator 2026-09-30): when the browser's own checks all pass on a behaviour change, the checks are the verdict and NO image is sent to the judge (tokens saved);
// when a check fails, the judge is told it failed and IS shown the picture of why.
#[tokio::test(flavor = "multi_thread")]
async fn j16e_the_browsers_own_checks_decide_behaviour_and_pictures_go_to_the_judge_only_when_needed() {
    if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome here - skipped"); return }
    for (tag, marker, images_expected) in [("tag-j16e-pass", "FAKE:checks", false), ("tag-j16e-fail", "FAKE:checks-fail", true)] {
        let _s = serial(); let app = app();
        let (pane, wt) = new_chat(&app).await; git_box(&wt);
        let state = std::path::Path::new(&wt).join("../fake-state-j16e"); std::env::set_var("FAKE_SEAT_STATE_DIR", &state);
        queue_add(&app, pane, &wt, &format!("create the search page FAKE:write:index.html {marker} {tag}")).await;
        let run = run_until(&app, pane, tag, "the run to be done", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false)).await;
        let events: Vec<String> = run["events"].as_array().unwrap().iter().map(|e| format!("{}: {}", e["kind"].as_str().unwrap_or(""), e["text"].as_str().unwrap_or(""))).collect();
        let checks = events.iter().find(|e| e.starts_with("qa-evidence") && e.contains("checks:")).unwrap_or_else(|| panic!("the run logged its checks: {events:?}"));
        let claim = std::fs::read_to_string(state.join("claim-prompt.txt")).expect("a claim check ran");
        if images_expected {
            assert!(checks.contains("1 failed") && checks.contains("the judge looks too"), "{checks}");
            assert!(claim.contains("FAIL ") && claim.contains("IMAGE page:"), "a failed check reaches the judge, with the picture of why: {claim}");
        } else {
            assert!(checks.contains("2 passed, 0 failed") && checks.contains("no image is sent"), "{checks}");
            assert!(claim.contains("PASS ") && !claim.contains("IMAGE page:") && claim.contains("no images attached"), "all checks passed: no picture is sent: {claim}");
        }
    }
}

// J17 - THE BACKEND'S QA FILM (operator 2026-09-30: "if the task i asked is a backend task is there a way to make also the QA test and display something similar"): a server change is
// exercised by an API test the QA wrote - requests to the box's own server, checks decided by the runner - streamed to the same viewer, and the judge is told which checks failed.
#[tokio::test(flavor = "multi_thread")]
async fn j17_a_backend_change_is_tested_through_the_servers_api_and_the_film_shows_the_exchanges() {
    if antiloki_serve::proc::which("node").is_none() || antiloki_serve::proc::which("npm").is_none() { eprintln!("no node/npm - skipped"); return }
    for (tag, marker, failed) in [("tag-j17-pass", "", 0usize), ("tag-j17-fail", "FAKE:api-fail", 1usize)] {
        let _s = serial(); let app = app();
        let (pane, wt) = new_chat(&app).await;
        let w = std::path::Path::new(&wt);
        std::fs::create_dir_all(w.join("node_modules")).unwrap();
        std::fs::write(w.join("package.json"), r#"{"scripts":{"dev":"node server.js"}}"#).unwrap();
        std::fs::write(w.join("server.js"), r#"const http=require("http");http.createServer((q,r)=>{r.setHeader("content-type","application/json");if(q.url==="/api/hello"){r.end(JSON.stringify({message:"hello from the box"}))}else{r.statusCode=404;r.end(JSON.stringify({error:"not found"}))}}).listen(0,"127.0.0.1",function(){console.log("listening on http://127.0.0.1:"+this.address().port)})"#).unwrap();
        std::fs::write(w.join(".gitignore"), "node_modules\n").unwrap();
        git_box(&wt);
        let state = w.join("../fake-state-j17"); std::env::set_var("FAKE_SEAT_STATE_DIR", &state);
        queue_add(&app, pane, &wt, &format!("create the hello handler FAKE:write:src/handler.js {marker} {tag}")).await;
        let run = run_until(&app, pane, tag, "the run to be done", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false)).await;
        let events: Vec<String> = run["events"].as_array().unwrap().iter().map(|e| format!("{}: {}", e["kind"].as_str().unwrap_or(""), e["text"].as_str().unwrap_or(""))).collect();
        let line = events.iter().find(|e| e.starts_with("qa-evidence") && e.contains("api test")).unwrap_or_else(|| panic!("the run logged its API test: {events:?}"));
        assert!(line.contains(&format!("{failed} failed")) && line.contains("decided by the runner"), "{line}");
        let claim = std::fs::read_to_string(state.join("claim-prompt.txt")).expect("a claim check ran");
        assert!(claim.contains("API EVIDENCE") && claim.contains("GET /api/hello -> 200"), "the claim check was handed what the server answered: {claim}");
        assert_eq!(claim.contains("FAIL "), failed == 1, "a failed check reaches the judge, a clean run carries none: {claim}");
        // the viewer's door: the film of the exchanges (snapshot carries the timeline; the kept film file has it too)
        let id = run["id"].as_i64().unwrap();
        let tl = std::fs::read_to_string(w.join(".antiloki/qa/evidence").join(format!("run-{id}")).join("film/timeline.json")).expect("the film's timeline is kept");
        assert!(tl.contains("\"exchange\"") && tl.contains("/api/hello") && tl.contains("hello from the box"), "{tl}");
    }
}

// J18 - THE ONLINE REPO IS NEVER CREATED BY ACCIDENT (operator 2026-09-30: the GitHub connector). Publishing makes a repository on the operator's own account, so without an explicit confirm it
// is refused, a pane that does not exist is refused, and the preflight answers for a real agent. (The positive path is not run here: `gh` is logged in on the dev machine and would create a real repo.)
#[tokio::test(flavor = "multi_thread")]
async fn j18_publishing_needs_an_explicit_confirm_and_the_preflight_answers() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/deliver/publish", Some(json!({ "pane_id": pane }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}"); assert!(v["error"].as_str().unwrap_or("").contains("explicit confirm"), "{v}");
    let (st, _) = call(&app, "POST", "/api/v1/cockpit/deliver/publish", Some(json!({ "pane_id": 99999, "confirm": true }))).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "a pane that does not exist");
    let (st, _) = call(&app, "GET", "/api/v1/cockpit/deliver/preflight?pane_id=99999", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, v) = call(&app, "GET", &format!("/api/v1/cockpit/deliver/preflight?pane_id={pane}"), None).await;
    assert!(st.is_success(), "{v}");      // with no workspace open it says so; with one it says what a PR would do - either way it answers
    assert!(v["ok"] == json!(false) && v["error"].is_string() || v["kind"].is_string(), "{v}");
    // F-01 (audit 2026-10-01): a confirm alone no longer publishes - the preflight's one-time nonce must come back with it
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/deliver/publish", Some(json!({ "pane_id": pane, "confirm": true }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}"); assert!(v["error"].as_str().unwrap_or("").contains("preflight"), "{v}");
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/deliver/publish", Some(json!({ "pane_id": pane, "confirm": true, "nonce": "not-the-nonce" }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}"); assert!(v["error"].as_str().unwrap_or("").contains("nonce"), "{v}");
}

// J18b - THE LOCAL DOOR (audit 2026-10-01, F-01): a web page from another origin cannot POST to the serve (the text/plain "simple request" needs no preflight), and a request under a foreign
// Host (DNS rebinding) is refused even as a GET; callers that send no Origin (curl, the CLIs, the shell) are untouched.
#[tokio::test(flavor = "multi_thread")]
async fn j18b_a_foreign_origin_or_host_cannot_reach_the_serve() {
    let _s = serial(); let app = app();
    let send = |method: &'static str, uri: &'static str, headers: Vec<(&'static str, &'static str)>, body: &'static str| { let app = app.clone(); async move {
        let mut b = Request::builder().method(method).uri(uri);
        for (k, v) in headers { b = b.header(k, v); }
        let res = app.oneshot(b.body(Body::from(body)).unwrap()).await.unwrap();
        (res.status(), res.headers().get("access-control-allow-origin").and_then(|v| v.to_str().ok()).map(str::to_string))
    } };
    let evil = r#"{"pane_id":1,"confirm":true}"#;
    let (st, _) = send("POST", "/api/v1/cockpit/deliver/publish", vec![("origin", "http://evil.example"), ("content-type", "text/plain")], evil).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "a cross-site text/plain POST never reaches the handler");
    let (st, _) = send("GET", "/api/v1/cockpit/deliver/preflight?pane_id=1", vec![("host", "evil.example:41931")], "").await;
    assert!(st == StatusCode::FORBIDDEN || st == StatusCode::UNAUTHORIZED, "DNS rebinding: a request that names another host is refused (it is a stranger: it has no device token), got {st}");
    let (st, acao) = send("POST", "/api/v1/cockpit/deliver/publish", vec![("origin", "http://localhost:5173"), ("content-type", "application/json"), ("host", "127.0.0.1:41931")], evil).await;
    assert_ne!(st, StatusCode::FORBIDDEN, "the app's own origin is served");
    assert_eq!(acao.as_deref(), Some("http://localhost:5173"));
    let (st, _) = send("POST", "/api/v1/cockpit/deliver/publish", vec![("content-type", "text/plain")], evil).await;
    assert_ne!(st, StatusCode::FORBIDDEN, "no Origin = not a browser page: untouched");
}

// J16c - BEFORE and AFTER: the same steps are run on the version the run started from, and the judge is told what the change did to each picture.
#[tokio::test(flavor = "multi_thread")]
async fn j16c_the_judge_is_shown_the_page_before_and_after_and_told_they_differ() {
    if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome here - skipped"); return }
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await;
    git(&wt, &["init", "-q"]);
    std::fs::write(std::path::Path::new(&wt).join("index.html"), "<h1 style='font:40px sans-serif'>the old landing page</h1>").unwrap();
    git(&wt, &["add", "-A"]); git(&wt, &["commit", "-q", "-m", "old page"]);
    let state = std::path::Path::new(&wt).join("../fake-state-j16c"); std::env::set_var("FAKE_SEAT_STATE_DIR", &state);
    queue_add(&app, pane, &wt, "create the new landing page FAKE:write:index.html tag-j16c").await;
    let run = run_until(&app, pane, "tag-j16c", "the run to be done", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false)).await;
    let events: Vec<String> = run["events"].as_array().unwrap().iter().map(|e| format!("{}: {}", e["kind"].as_str().unwrap_or(""), e["text"].as_str().unwrap_or(""))).collect();
    let line = events.iter().find(|e| e.starts_with("qa-evidence")).unwrap_or_else(|| panic!("evidence logged: {events:?}"));
    assert!(line.contains("vs the old version: 0 identical / 1 differ"), "{line}");
    let claim = std::fs::read_to_string(state.join("claim-prompt.txt")).expect("a claim check ran");
    assert!(claim.contains("IMAGE before-page:") && claim.contains("page: DIFFERS from before"), "the claim check was told what changed: {claim}");
    let dir = std::path::Path::new(&wt).join(".antiloki/qa/evidence").join(format!("run-{}", run["id"]));
    assert!(dir.join("01-page.png").exists() && dir.join("base/01-page.png").exists(), "both pictures are kept");
    assert!(!dir.join("base-src").exists(), "the materialized old version is cleaned up");
}

// J16d - THE QA, LIVE: a viewer that joins gets the play-by-play (a snapshot with the step timeline and the newest frame), the kept film can be fetched frame by frame for a replay,
// and nothing outside the film folder can be reached.
#[tokio::test(flavor = "multi_thread")]
async fn j16d_a_viewer_gets_the_qa_play_by_play_and_the_film_can_be_replayed() {
    if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome here - skipped"); return }
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j16d"));
    queue_add(&app, pane, &wt, "create the live page FAKE:write:index.html tag-j16d").await;
    let run = run_until(&app, pane, "tag-j16d", "the run to be done", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false)).await;
    let id = run["id"].as_i64().unwrap();
    let res = axum::Router::clone(&app).oneshot(Request::builder().method("GET").uri(format!("/api/v1/cockpit/goals/live?id={id}")).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK); assert!(res.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let body = String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes()).to_string();
    let first: Value = serde_json::from_str(body.lines().find_map(|l| l.strip_prefix("data:")).expect("an event").trim()).unwrap();
    assert_eq!(first["type"], "snapshot"); assert_eq!(first["done"], true, "the run is over: nothing more to stream");
    let steps: Vec<&Value> = first["timeline"].as_array().unwrap().iter().filter(|e| e["type"] == "step").collect();
    assert!(!steps.is_empty() && steps[0]["text"].as_str().unwrap().starts_with("opening"), "the timeline says what it did: {first}");
    assert!(first["last_frame"]["jpeg"].as_str().map_or(false, |j| j.len() > 100), "and it keeps the newest frame for a late viewer");
    let film = first["film"].as_array().expect("the kept film's timeline rides along for the replay");
    let frame = film.iter().find(|e| e["type"] == "film").expect("a film frame")["file"].as_str().unwrap().to_string();
    let r = axum::Router::clone(&app).oneshot(Request::builder().method("GET").uri(format!("/api/v1/cockpit/goals/evidence?id={id}&film={frame}")).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK); assert_eq!(r.headers()["content-type"], "image/jpeg");
    assert_eq!(&r.into_body().collect().await.unwrap().to_bytes()[..2], &[0xFF, 0xD8]);
    let (st, _) = call(&app, "GET", &format!("/api/v1/cockpit/goals/evidence?id={id}&film=..%2F0001.jpg"), None).await; assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(&app, "GET", "/api/v1/cockpit/goals/live?id=999999", None).await; assert_eq!(st, StatusCode::NOT_FOUND);
}

// J19 - THE QA'S PLAN KEPT AS A REGRESSION TEST (item 8d, 2026-10-01). A run's QA writes a deterministic plan and the browser decides it; until now it lived for one run. A plan that PASSED
// can now be kept beside the code (`.antiloki/qa/<slug>.json`) and replayed any time, by the same runner with no model: it passes while the page still does what it did, and fails - naming the
// check and what the page had instead - the day the page stops. A plan that failed, or that only took pictures, is refused: it would not be a test.
#[tokio::test(flavor = "multi_thread")]
async fn j19_a_plan_that_passed_is_kept_replayed_and_fails_the_day_the_page_regresses() {
    if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome here - skipped"); return }
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j19"));
    let done = |r: &Value| r["status"] == json!("done") && r["qa_pending"] == json!(false);

    // a run whose plan PASSES (two deterministic checks on the page the worker wrote), and one whose plan FAILS (it asks for text the page does not have)
    queue_add(&app, pane, &wt, "create the search page FAKE:write:index.html FAKE:checks tag-j19-pass").await;
    let pass = run_until(&app, pane, "tag-j19-pass", "the passing run to be done", done).await;
    let pass_id = pass["id"].as_i64().unwrap();
    // the run left its plan beside its pictures, with how it ended - nothing is promoted by itself
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(std::path::Path::new(&wt).join(format!(".antiloki/qa/evidence/run-{pass_id}/plan.json"))).expect("the run recorded its plan")).unwrap();
    assert_eq!((rec["kind"].as_str(), rec["passed"].as_bool(), rec["checks"].as_u64(), rec["checks_failed"].as_u64()), (Some("ui"), Some(true), Some(2), Some(0)), "{rec}");
    assert!(rec["steps"].as_array().unwrap().iter().any(|s| s["do"] == "expect" && s["text"] == "written by the fake seat"), "the plan that ran is in the record: {}", rec["steps"]);
    assert!(!std::path::Path::new(&wt).join(".antiloki/qa/plans").exists() && call(&app, "GET", &format!("/api/v1/cockpit/qa/plans?cwd={wt}"), None).await.1["plans"].as_array().unwrap().is_empty(), "nothing is saved until somebody saves it");

    // a plan that failed is a bug report, not a regression test
    queue_add(&app, pane, &wt, "create the other page FAKE:write:other.html FAKE:checks-fail tag-j19-fail").await;
    let fail = run_until(&app, pane, "tag-j19-fail", "the failing run to be done", done).await;
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/qa/plans", Some(json!({ "run_id": fail["id"], "slug": "never" }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"]["message"].as_str().unwrap_or("").contains("did not pass"), "{v}");
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/qa/plans", Some(json!({ "run_id": 999_999 }))).await; assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    let (st, _) = call(&app, "POST", "/api/v1/cockpit/qa/plans", Some(json!({ "run_id": pass_id, "slug": "../escape" }))).await; assert_eq!(st, StatusCode::BAD_REQUEST, "a name is not a path");

    // kept: where it was promised, readable, and listed
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/qa/plans", Some(json!({ "run_id": pass_id, "slug": "search-page" }))).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!((v["plan"]["slug"].as_str(), v["plan"]["kind"].as_str(), v["plan"]["checks"].as_u64()), (Some("search-page"), Some("ui"), Some(2)), "{v}");
    let file = std::path::Path::new(&wt).join(".antiloki/qa/search-page.json");
    assert!(file.is_file(), "{}", file.display());
    let listed = call(&app, "GET", &format!("/api/v1/cockpit/qa/plans?cwd={wt}"), None).await.1;
    assert_eq!(listed["plans"].as_array().unwrap().iter().map(|p| p["slug"].as_str().unwrap()).collect::<Vec<_>>(), vec!["search-page"], "{listed}");

    // F-14/F-15 (audit 2026-10-01): a kept plan is not replaced silently; a folder that is not the workspace's (or one of its runs') is not a place a door works in; a replay goes to loopback only
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/qa/plans", Some(json!({ "run_id": pass_id, "slug": "search-page" }))).await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}"); assert!(v["error"]["message"].as_str().unwrap_or("").contains("already exists"), "{v}");
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/qa/plans", Some(json!({ "run_id": pass_id, "slug": "search-page", "overwrite": true }))).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let (st, v) = call(&app, "GET", "/api/v1/cockpit/qa/plans?cwd=/etc", None).await; assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/qa/replay", Some(json!({ "slug": "search-page", "cwd": "/etc" }))).await; assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/qa/plans", Some(json!({ "slug": "search-page", "cwd": "/etc", "remove": true }))).await; assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/qa/replay", Some(json!({ "slug": "search-page", "cwd": wt, "app": "http://evil.example/" }))).await; assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"]["message"].as_str().unwrap_or("").contains("not on this machine"), "{v}");

    // replayed against the page as it is: still passes, both checks, no model
    let (st, ok) = call(&app, "POST", "/api/v1/cockpit/qa/replay", Some(json!({ "slug": "search-page", "cwd": wt }))).await;
    assert_eq!(st, StatusCode::OK, "{ok}");
    assert_eq!((ok["passed"].as_bool(), ok["checks_passed"].as_u64(), ok["checks_failed"].as_u64(), ok["failed_steps"].as_u64()), (Some(true), Some(2), Some(0), Some(0)), "{ok}");
    assert_eq!(ok["source"], json!("a static page in the folder"));

    // the page regresses: the replay fails, says which check, and what the page said instead
    std::fs::write(std::path::Path::new(&wt).join("index.html"), "<!doctype html><title>shop</title><body>a page about zebra crossings</body>").unwrap();
    let (_, bad) = call(&app, "POST", "/api/v1/cockpit/qa/replay", Some(json!({ "slug": "search-page", "cwd": wt }))).await;
    assert_eq!(bad["passed"], json!(false), "{bad}");
    let why = bad["why"].as_array().unwrap().iter().map(|w| w.as_str().unwrap().to_string()).collect::<Vec<_>>().join(" | ");
    assert!(why.contains("written by the fake seat") && why.contains("check failed"), "the failure names the check: {why}");
    assert!(why.contains("zebra"), "...and what the page had instead: {why}");
    assert_eq!((bad["checks_passed"].as_u64(), bad["checks_failed"].as_u64()), (Some(0), Some(2)), "both asked for what is gone (the second, 'no zebra', now fails too): {bad}");

    // an unknown plan is said so, and a deleted one is gone
    let (st, v) = call(&app, "POST", "/api/v1/cockpit/qa/replay", Some(json!({ "slug": "nope", "cwd": wt }))).await; assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"]["message"].as_str().unwrap_or("").contains("no saved QA plan"), "{v}");
    let (st, _) = call(&app, "POST", "/api/v1/cockpit/qa/plans", Some(json!({ "slug": "search-page", "cwd": wt, "remove": true }))).await; assert_eq!(st, StatusCode::OK);
    assert!(!file.exists());
}

// J9c - THE UNIT UNDER J9b (2026-10-01): A WORKSPACE SWITCH DOES NOT WAIT FOR A BUSY CONNECTION. `rebind` (called by `workspaces/activate`) used to `lock()` every connection in turn, so one pool member held by a long engine
// action (the rebuild of the workspace being left: minutes on a big tree) held the switch for as long. Here a "long action" holds one pool connection for 2.5 s: the rebind must return at once having swapped the
// others, must not hand the busy one out to a dispatch while it waits, and must swap it the moment the action lets go.
#[test]
fn j9c_a_workspace_switch_does_not_wait_for_a_connection_that_is_busy_and_swaps_it_when_it_is_free() {
    let _s = serial();
    let st = fresh_state(2);
    let held = st.pool[0].clone();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let long_action = std::thread::spawn(move || { let _g = held.lock().unwrap(); tx.send(()).unwrap(); std::thread::sleep(std::time::Duration::from_millis(2500)); });
    rx.recv().unwrap();
    let t0 = std::time::Instant::now();
    let n = st.rebind().expect("rebind");
    assert!(t0.elapsed() < std::time::Duration::from_millis(2200), "the switch waited {:?} for a busy connection", t0.elapsed());   // the held connection is held for 2.5 s: a switch that waited for it takes at least that; the limit sits below it, with room for three database opens on a loaded machine (1.5 s was too tight in a debug build, 2026-11-19)
    assert_eq!(n, 2, "the primary and the free pool member are rebound now; the busy one is not counted");
    assert_eq!(antiloki_serve::dispatch::rebinds_pending(), 1, "the busy one is waiting for its action to end");
    for _ in 0..50 { assert!(!Arc::ptr_eq(&st.pick(), &st.pool[0]), "a connection that is waiting to be rebound is never handed to a dispatch"); }
    // rebinding again while it waits queues nothing new
    st.rebind().expect("a second switch");
    assert_eq!(antiloki_serve::dispatch::rebinds_pending(), 1);
    long_action.join().unwrap();
    let t1 = std::time::Instant::now();
    while antiloki_serve::dispatch::rebinds_pending() > 0 && t1.elapsed() < std::time::Duration::from_secs(10) { std::thread::sleep(std::time::Duration::from_millis(50)); }
    assert_eq!(antiloki_serve::dispatch::rebinds_pending(), 0, "the busy connection was rebound once its action let go");
    let one: i64 = st.pool[0].lock().unwrap().query_row("SELECT 1", [], |r| r.get(0)).expect("the swapped connection works");
    assert_eq!(one, 1);
}

// J18 - THE DESIGN ROUND (PLAN-THE-DESIGN S5 / D4, operator 2026-10-01: Claude's page looked clearly better than Composer's and nothing measured it). After the claims are met, ONE critic with eyes writes at most five
// fixes from the page's screenshots and design lint; they go back to the builder as ONE send-back brief, inside the QA cap, and the page is measured again. Taste never holds a run back; a D1 rule failure does.
// The critic is the scripted claude (ANTILOKI_DESIGN_CRITIC pins it - the box must never call a real seat). Needs the machine's Chrome (skips without one).
/// While alive, the design round follows `how` (an ANTILOKI_QA_DESIGN value, or "file" = the settings file decides); dropped, the journeys' default (off) is back - even if the test panicked.
struct DesignOn(&'static str);
impl DesignOn { fn new(how: &'static str) -> Self { if how == "file" { std::env::remove_var("ANTILOKI_QA_DESIGN"); } else { std::env::set_var("ANTILOKI_QA_DESIGN", how); } DesignOn(how) } }
impl Drop for DesignOn { fn drop(&mut self) { std::env::set_var("ANTILOKI_QA_DESIGN", "off"); } }
fn design_run_events(run: &Value) -> Vec<String> { run["events"].as_array().unwrap().iter().map(|e| format!("{}: {}", e["kind"].as_str().unwrap_or(""), e["text"].as_str().unwrap_or(""))).collect() }
fn counter(state: &std::path::Path, name: &str) -> u32 { std::fs::read_to_string(state.join(name)).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0) }
fn builder_briefs(state: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(state.join("worker-prompts.txt")).unwrap_or_default().split("=====WORKER PROMPT=====").filter(|b| b.contains("index.html")).map(String::from).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn j18a_the_design_round_runs_once_sends_the_critics_fixes_back_and_delivers_with_design_notes() {
    if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome here - the design round journey is skipped"); return }
    let _s = serial(); let app = app(); let _d = DesignOn::new("auto"); // (after app(): its first call sets the journeys' default and would overwrite the guard)
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    let state = std::path::Path::new(&wt).join("../fake-state-j18a"); std::env::set_var("FAKE_SEAT_STATE_DIR", &state);
    // the page passes every rule (it is a good page); the critic first answers in prose, so the re-ask has to happen too
    queue_add(&app, pane, &wt, "create the shop page FAKE:write:index.html FAKE:html:good FAKE:critic-prose tag-j18a").await;
    let run = run_until(&app, pane, "tag-j18a", "the run to be delivered after its design round", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false) && r["qa_rounds"] == json!(1)).await;
    let events = design_run_events(&run);
    let all = format!("{events:#?}");
    assert_eq!(counter(&state, "critique"), 2, "the critic was asked once, re-asked once for JSON, and NOT asked again on the second pass: {all}");
    assert!(events.iter().any(|e| e.starts_with("design-round: design round: claude-code/sonnet looks at")), "the round is in the story: {all}");
    assert!(events.iter().any(|e| e.starts_with("design-round: design round: 3 fix(es) from claude-code/sonnet, +$")), "the story names the seat, the number of fixes and the cost: {all}");
    assert!(events.iter().any(|e| e.starts_with("design-notes: #1 [spacing] the product grid")), "the fixes are numbered lines in the story: {all}");
    assert!(events.iter().any(|e| e.starts_with("design-round: after the design round the page is measured again: design lint")), "the page was measured again: {all}");
    // the builder's second brief carries the fixes - and, the page being fine, NO rule
    let briefs = builder_briefs(&state);
    assert_eq!(briefs.len(), 2, "built twice: first the ask, then the design round");
    assert!(briefs[1].contains("design fix #1 [spacing] the product grid") && briefs[1].contains("increase the gap between the toolbar and the grid to 24px") && briefs[1].contains("design fix #3"), "{}", briefs[1]);
    assert!(!briefs[0].contains("design fix") && !briefs[1].contains("this blocks delivery"), "taste is polish, not a blocker");
    // the design state, the cost and the report
    let d = &run["design"];
    assert_eq!((d["rounds"].clone(), d["sent"].clone(), d["seat"].clone()), (json!(1), json!(true), json!("claude-code/sonnet")), "{d}");
    assert!(d["usd"].as_f64().unwrap_or(0.0) > 0.0, "the critic is priced, never a zero: {d}");
    assert!(run["qa_gate"]["cost"].as_f64().unwrap_or(0.0) >= d["usd"].as_f64().unwrap_or(1.0), "the critic's cost is in gate.cost: {}", run["qa_gate"]);
    assert!(run["report"].as_str().unwrap_or("").contains("DESIGN NOTES (design round: 3 fix(es) from claude-code/sonnet"), "the report carries the design notes: {}", run["report"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn j18b_a_contrast_failure_is_a_rule_it_goes_back_with_the_fixes_and_the_rebuilt_page_is_measured_higher() {
    if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome here - the design round journey is skipped"); return }
    let _s = serial(); let app = app(); let _d = DesignOn::new("auto"); // (after app(): its first call sets the journeys' default and would overwrite the guard)
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    let state = std::path::Path::new(&wt).join("../fake-state-j18b"); std::env::set_var("FAKE_SEAT_STATE_DIR", &state);
    // built with grey #a3a3a3 text on white (2.52:1); fixed once the brief says a design rule failed
    queue_add(&app, pane, &wt, "create the shop page FAKE:write:index.html FAKE:html:lowcontrast tag-j18b").await;
    let run = run_until(&app, pane, "tag-j18b", "the run to be delivered", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false) && r["qa_rounds"] == json!(1)).await;
    let all = format!("{:#?}", design_run_events(&run));
    let briefs = builder_briefs(&state);
    assert_eq!(briefs.len(), 2, "one send-back, not two: the rule and the fixes share ONE brief");
    assert!(briefs[1].contains("design rule (this blocks delivery) - text contrast:") && briefs[1].contains("#a3a3a3") && briefs[1].contains("2.52:1"), "the rule names the node and the number: {}", briefs[1]);
    assert!(briefs[1].contains("design fix #1"), "and the critic's fixes ride along: {}", briefs[1]);
    assert!(briefs[1].find("design rule").unwrap() < briefs[1].find("design fix #1").unwrap(), "the rule comes first");
    assert_eq!(counter(&state, "critique"), 1);
    let d = &run["design"];
    let (before, after) = (d["score_first"].as_f64().unwrap_or(9.0), d["score"].as_f64().unwrap_or(-1.0));
    assert!(before <= 0.75 && after >= 0.95 && after > before, "the second capture scores higher on the lint: {before} -> {after}\n{all}");
    assert_eq!(d["blockers"], json!([]), "nothing blocks the rebuilt page: {d}");
    assert!(run["report"].as_str().unwrap_or("").contains(&format!("design lint {before:.2} -> {after:.2}")), "the report says before and after: {}", run["report"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn j18c_a_rule_that_is_still_broken_when_the_cap_is_spent_parks_the_run_on_qas_question_and_the_critic_is_not_asked_twice() {
    if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome here - the design round journey is skipped"); return }
    let _s = serial(); let app = app(); let _d = DesignOn::new("auto"); // (after app(): its first call sets the journeys' default and would overwrite the guard)
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    let state = std::path::Path::new(&wt).join("../fake-state-j18c"); std::env::set_var("FAKE_SEAT_STATE_DIR", &state);
    queue_add(&app, pane, &wt, "create the shop page FAKE:write:index.html FAKE:html:stuck tag-j18c").await;
    let run = run_until(&app, pane, "tag-j18c", "the run to park on QA's question", awaiting).await;
    assert_eq!(run["qa_rounds"], json!(2), "the cap, never more: {}", run["qa_rounds"]);
    assert_eq!(counter(&state, "critique"), 1, "ONE design round per run: the second send-back is the rule alone");
    let q = &run["questions"][0];
    assert_eq!(q["kind"], json!("qa-direction"), "{q}");
    assert!(q["text"].as_str().unwrap_or("").contains("design rule") && q["text"].as_str().unwrap_or("").contains("text contrast"), "the question says which rule is still broken: {q}");
    assert_ne!(run["delivered"], json!(true), "a page that cannot be read is not delivered");
}

#[tokio::test(flavor = "multi_thread")]
async fn j18d_the_setting_turns_the_whole_design_round_off_and_the_page_is_delivered_as_built() {
    if antiloki_rs::codebase_helpers_browser_capture::find_browser().is_err() { eprintln!("no Chrome here - the design round journey is skipped"); return }
    let _s = serial(); let app = app(); let _d = DesignOn::new("file"); // (after app(): its first call sets the journeys' default and would overwrite the guard)
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    let (st, v) = call(&app, "GET", "/api/v1/settings/qa", None).await; assert!(st.is_success() && v["design"] == json!("auto"), "default is auto: {v}");
    let (st, v) = call(&app, "POST", "/api/v1/settings/qa", Some(json!({ "design": "off" }))).await; assert!(st.is_success() && v["design"] == json!("off"), "{v}");
    let (st, _) = call(&app, "POST", "/api/v1/settings/qa", Some(json!({ "design": "maybe" }))).await; assert_eq!(st, StatusCode::BAD_REQUEST);
    let state = std::path::Path::new(&wt).join("../fake-state-j18d"); std::env::set_var("FAKE_SEAT_STATE_DIR", &state);
    queue_add(&app, pane, &wt, "create the shop page FAKE:write:index.html FAKE:html:lowcontrast tag-j18d").await;
    let run = run_until(&app, pane, "tag-j18d", "the run to be delivered", |r| r["status"] == json!("done") && r["qa_pending"] == json!(false)).await;
    assert_eq!(run["qa_rounds"], json!(0), "off: no send-back"); assert_eq!(counter(&state, "critique"), 0, "off: the critic is never asked");
    assert!(!run["report"].as_str().unwrap_or("").contains("DESIGN NOTES"), "{}", run["report"]);
}

/// A PATH that reaches ONLY scripted seats (plus the system's python3 and git): the operator's real `agy`, `codex`, `cursor-agent` and `grok` live on the
/// developer's PATH, and a journey that switches Claude off must not be able to spend a real seat's quota. Restores everything on drop, panic included.
struct ScriptedSeatsOnly { path: Option<String>, vars: Vec<&'static str> }
impl ScriptedSeatsOnly {
    fn new(bin: &std::path::Path, calls_log: &std::path::Path) -> Self {
        let path = std::env::var("PATH").ok();
        std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.display()));
        std::env::set_var("CURSOR_API_KEY", "scripted"); std::env::set_var("FAKE_SEAT_CALLS_LOG", calls_log);
        antiloki_serve::cli::forget_seat_probes();   // the PATH just changed: a "not installed" answer cached for 10 s by the journey before would still be believed
        ScriptedSeatsOnly { path, vars: vec!["CURSOR_API_KEY", "FAKE_SEAT_CALLS_LOG"] }
    }
}
impl Drop for ScriptedSeatsOnly {
    fn drop(&mut self) {
        match &self.path { Some(p) => std::env::set_var("PATH", p), None => std::env::remove_var("PATH") }
        antiloki_serve::cli::forget_seat_probes();
        for v in &self.vars { std::env::remove_var(v); }
    }
}

// J20 - CLAUDE IS GONE (operator 2026-10-05: "the plan was preparing to not having Max account on Claude"). With the Claude seat switched off and Cursor the only seat on the machine, an ask still goes
// through the whole pipeline and lands in the box - and the scripted `claude` is NEVER started, by any role (screen, planner, ratifier, worker, verifier, QA gate, join). Until this journey nothing
// asserted it: the only scripted seat was Claude, so "Claude-off works" was a reading of the code, not a run.
#[tokio::test(flavor = "multi_thread")]
async fn j20_with_claude_switched_off_the_whole_pipeline_runs_on_another_seat_and_claude_is_never_called() {
    let _s = serial(); let app = app();
    let root = env_once();
    let bin = root.join("bin");
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude");
    let cursor = bin.join("cursor-agent"); if !cursor.exists() { std::os::unix::fs::symlink(&fake, &cursor).unwrap(); }
    let calls = root.join(format!("calls-j20-{}.log", std::process::id())); let _ = std::fs::remove_file(&calls);
    let _seats = ScriptedSeatsOnly::new(&bin, &calls);
    let (st, v) = call(&app, "POST", "/api/v1/settings/providers", Some(json!({ "name": "claude-code", "on": false }))).await;
    assert!(st.is_success(), "switching Claude off: {st} {v}");
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j20"));
    queue_add(&app, pane, &wt, "create the greeting file FAKE:write:out/hello.txt tag-j20").await;
    let run = run_until(&app, pane, "tag-j20", "the run to finish", |r| matches!(r["status"].as_str(), Some("done" | "failed" | "cancelled" | "error"))).await;
    let seen = std::fs::read_to_string(&calls).unwrap_or_default();
    let names: Vec<&str> = seen.lines().collect();
    assert!(!names.iter().any(|n| *n == "claude"), "Claude was called although it is switched off: {names:?}");
    assert!(names.iter().any(|n| *n == "cursor-agent"), "the other seat did the work: {names:?}");
    assert_eq!(run["status"], json!("done"), "the run: {}", serde_json::to_string_pretty(&run).unwrap_or_default());
    assert!(std::path::Path::new(&wt).join("out/hello.txt").exists(), "the worker's file is in the box; run: {}", serde_json::to_string_pretty(&run).unwrap_or_default());
}

// J32 - THE FIRST-RUN CHECK (operator 2026-10-11: "before any use all providers CLIs must be tested and gather the weeks usage of all on antiloki, so the user already starts seeing it at the
// switcher"). One seat per call: one tiny real call through the same runner every job uses, and what the seat says about the week. The scripted claude prints its plan windows in the stream
// (FAKE_RATE_LIMIT), which the check writes where the meter reader looks, so the switcher's source - `account_windows` - shows 12 % of the 5-hour window and 41 % of the week without the user
// having spent anything through antiloki. A seat that is not on the machine says "install" and spends nothing.
#[tokio::test(flavor = "multi_thread")]
async fn j32_the_first_run_check_tests_a_seat_with_one_real_call_and_reads_its_week() {
    let _g = serial(); let app = app();
    std::fs::create_dir_all(std::env::var("ANTILOKI_SUBSTRATE_ROOT").unwrap()).unwrap();   // the workspace folder: a fresh test substrate has not made it yet, and a call cannot start in a folder that is not there
    std::env::set_var("FAKE_RATE_LIMIT", "1");
    let (st, v) = call(&app, "POST", "/api/v1/onboarding/seat-check", Some(json!({ "seat": "claude-code" }))).await;
    std::env::remove_var("FAKE_RATE_LIMIT");
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["ok"], json!(true), "the scripted claude answers a tiny call: {v}");
    assert_eq!(v["stage"], json!("done")); assert!(v["said"].as_str().unwrap_or("").contains("ack"), "{v}");
    let ws = v["windows"].as_array().expect("windows"); let pct = |label: &str| ws.iter().find(|w| w["label"] == json!(label)).and_then(|w| w["used_pct"].as_f64());
    assert_eq!(pct("5h"), Some(12.0), "{v}"); assert_eq!(pct("week"), Some(41.0), "{v}");
    assert!(ws.iter().all(|w| w["house"] == json!("claude")), "only this seat's own windows: {v}");
    // the meter file is where the reader looks: the next read of the switcher's source finds the week without another call
    let root = std::env::var("ANTILOKI_SUBSTRATE_ROOT").unwrap();
    assert!(std::path::Path::new(&root).join(".antiloki").join("usage-0.json").exists(), "the plan windows were written to the workspace's meter");
    // a seat that is not on the machine is told to install, with no call made
    let (_, g) = call(&app, "POST", "/api/v1/onboarding/seat-check", Some(json!({ "seat": "grok" }))).await;
    assert_eq!(g["ok"], json!(false));
    // told to install only where grok really is absent: a machine that has it (a mise wrapper counts) is told to sign in, or answers
    if antiloki_rs::proc::which("grok").is_none() { assert_eq!(g["stage"], json!("install"), "{g}"); } else { assert_ne!(g["stage"], json!("install"), "{g}"); }
    // and a name that is not a seat is refused
    let (st3, _) = call(&app, "POST", "/api/v1/onboarding/seat-check", Some(json!({ "seat": "nope" }))).await;
    assert_eq!(st3, StatusCode::BAD_REQUEST);
}


// J33 - A CHAT BELONGS TO ITS PROJECT (operator 2026-10-11: "I opened antiloki and it showed a continuation of the same conversation in all projects"). Pane ids are numbered per project database, and the chat's folder (where its event
// log lives) was `chats/<pane id>` - one folder for every project. The folder is now `chats/<project key>/<pane id>`; a chat made before is given its own empty folder in its project when the chats are listed, and the shared
// folder - which holds every project's turns interleaved - is left exactly as it was.
#[tokio::test(flavor = "multi_thread")]
async fn j33_a_chat_lives_in_a_folder_of_its_own_project_and_an_old_shared_folder_is_not_inherited() {
    let _s = serial(); let st = fresh_state(0); let app = build_router(st.clone());
    let chats = PathBuf::from(std::env::var("ANTILOKI_CHATS_DIR").unwrap());
    let (pane, wt) = new_chat(&app).await;
    let wt = PathBuf::from(&wt);
    assert_eq!(wt.file_name().unwrap().to_string_lossy(), pane.to_string());
    assert_eq!(wt.parent().unwrap().parent().unwrap(), chats.as_path(), "the folder is chats/<project>/<pane>, not chats/<pane>: {}", wt.display());
    // a chat from before the folders were scoped: its row points at the shared folder, which holds a turn that belongs to some other project
    let legacy = chats.join(pane.to_string());
    std::fs::create_dir_all(legacy.join(".antiloki")).unwrap();
    let leaked = r#"{"seq":1,"t":1,"id":"x","ev":{"type":"antiloki","subtype":"user","text":"a turn from ANOTHER project"}}"#;
    std::fs::write(antiloki_serve::agent::events_read_path(&legacy.to_string_lossy(), pane), format!("{leaked}\n")).unwrap();
    st.conn.lock().unwrap().execute("UPDATE cockpit_panes SET worktree_path = ?1 WHERE id = ?2", rusqlite::params![legacy.to_string_lossy(), pane]).expect("point the chat at the old shared folder");
    // listing the chats adopts it into this project
    let (code, list) = call(&app, "GET", "/api/v1/cockpit/chats", None).await;
    assert_eq!(code, StatusCode::OK, "{list}");
    let mine = list["chats"].as_array().unwrap().iter().find(|c| c["pane_id"].as_i64() == Some(pane)).expect("the chat is listed").clone();
    assert_eq!(mine["turns"].as_i64().unwrap_or(0), 0, "the other project's turn is not this chat's history: {mine}");
    let now: String = st.conn.lock().unwrap().query_row("SELECT worktree_path FROM cockpit_panes WHERE id = ?1", [pane], |r| r.get(0)).unwrap();
    assert_eq!(PathBuf::from(now), wt, "the chat is back in its own project's folder");
    assert!(std::fs::read_to_string(antiloki_serve::agent::events_read_path(&legacy.to_string_lossy(), pane)).unwrap().contains("ANOTHER project"), "the shared folder is left untouched");
    assert!(events_of(&wt.to_string_lossy(), pane).is_empty() || !events_of(&wt.to_string_lossy(), pane).iter().any(|e| e.to_string().contains("ANOTHER project")));
}

// J34 - A PLAN BECOMES A SAGA (operator 2026-10-11): an ask goes through the real pipeline, and the project's Sagas screen shows it - a saga planned by Heimdall, its task done by a named seat, the telemetry the run recorded,
// the trail in the run's own words. Reading it twice adds nothing.
#[tokio::test(flavor = "multi_thread")]
async fn j34_a_goal_run_is_a_saga_with_its_tasks_its_seat_and_its_trail() {
    let _s = serial(); let app = app();
    let (pane, wt) = new_chat(&app).await; git_box(&wt);
    std::env::set_var("FAKE_SEAT_STATE_DIR", std::path::Path::new(&wt).join("../fake-state-j34"));
    queue_add(&app, pane, &wt, "create the greeting file FAKE:write:out/hello.txt tag-j34").await;
    let run = run_until(&app, pane, "tag-j34", "the run to finish", |r| matches!(r["status"].as_str(), Some("done" | "failed" | "cancelled" | "error"))).await;
    assert_eq!(run["status"], json!("done"), "the run: {}", serde_json::to_string_pretty(&run).unwrap_or_default());

    let (code, list) = call(&app, "GET", "/api/v1/sagas", None).await;
    assert_eq!(code, StatusCode::OK, "{list}");
    let mine = list["sagas"].as_array().unwrap().iter().find(|s| s["title"].as_str().map_or(false, |t| t.contains("greeting file"))).unwrap_or_else(|| panic!("the run is not among the sagas: {list}")).clone();
    assert_eq!(mine["origin"], json!("heimdall"));
    assert_eq!(mine["status"], json!("done"), "{mine}");
    let id = mine["id"].as_i64().unwrap();
    let (_, d) = call(&app, "GET", &format!("/api/v1/sagas/detail?id={id}"), None).await;
    let tasks = d["tasks"].as_array().unwrap();
    assert!(!tasks.is_empty() && tasks.iter().all(|t| t["status"] == json!("done")), "every task of a delivered run is done: {d}");
    assert!(tasks.iter().any(|t| !t["engine"].as_str().unwrap_or("").is_empty()), "a task names the seat that did it: {d}");
    assert!(d["events"].as_array().unwrap().iter().any(|e| e["actor"] == json!("heimdall")), "the trail has the run's own story: {d}");
    // reading again changes nothing (the sync is idempotent) - and a task of the user's own can be added to the plan's saga
    let n_events = d["events"].as_array().unwrap().len();
    let (_, d2) = call(&app, "GET", &format!("/api/v1/sagas/detail?id={id}"), None).await;
    assert_eq!(d2["events"].as_array().unwrap().len(), n_events, "a second read adds no events");
    let (st, t) = call(&app, "POST", "/api/v1/sagas/task-create", Some(json!({ "saga_id": id, "title": "review the greeting by hand" }))).await;
    assert_eq!(st, StatusCode::OK, "{t}");
    let (_, d3) = call(&app, "GET", &format!("/api/v1/sagas/detail?id={id}"), None).await;
    assert_eq!(d3["tasks"].as_array().unwrap().len(), tasks.len() + 1);
    assert_eq!(d3["saga"]["status"], json!("doing"), "a new open task reopens a finished saga");
}

// J-X2 - X-RAY V2 END TO END (RFC 0015, the operator 2026-11-18): a workspace, its widgets as declarations, the screen's door and the agents' MCP door on the SAME store: an AI source is refused, a bad layout saves nothing,
// every change is a version that can be undone, an agent can propose a package but only the person installs it, and the same declaration always draws the same data.
#[tokio::test(flavor = "multi_thread")]
async fn jx2_xray_widgets_are_declarations_drawn_deterministically_and_changed_through_one_store() {
    let _s = serial(); let app = app();
    let root = env_once().join("xray2-ws"); let _ = std::fs::remove_dir_all(&root); std::fs::create_dir_all(&root).unwrap(); let r = root.to_string_lossy().to_string();
    for args in [vec!["init", "-q"], vec!["config", "user.email", "t@t"], vec!["config", "user.name", "t"]] { assert!(std::process::Command::new("git").args(&args).current_dir(&root).status().unwrap().success()); }
    std::fs::write(root.join("a.txt"), "one\n").unwrap(); assert!(std::process::Command::new("git").args(["add", "."]).current_dir(&root).status().unwrap().success()); assert!(std::process::Command::new("git").args(["commit", "-qm", "first"]).current_dir(&root).status().unwrap().success());
    let (_, v) = call(&app, "POST", "/api/v1/workspaces/add", Some(json!({ "name": "xray2", "root_path": r }))).await; assert_eq!(v["ok"], json!(true), "{v}");
    let (_, v) = call(&app, "POST", "/api/v1/workspaces/activate", Some(json!({ "name": "xray2" }))).await; assert_eq!(v["active"], json!("xray2"), "{v}");
    let x2 = |op: &str, extra: Value| { let mut b = json!({ "op": op }); if let (Some(o), Some(e)) = (b.as_object_mut(), extra.as_object()) { for (k, v) in e { o.insert(k.clone(), v.clone()); } } b };
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("widgets", json!({})))).await; assert_eq!(v["ok"], json!(true), "{v}"); assert!(v["widgets"].as_array().unwrap().len() >= 10, "the base widgets are declarations: {v}"); assert!(v["widgets"].as_array().unwrap().iter().all(|w| w["base"] == json!(true)));
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("sources", json!({})))).await; assert!(v["sources"].as_array().unwrap().iter().all(|s| s["key"] != json!("describe")), "no AI source in the palette"); assert!(v["renderers"].as_array().unwrap().contains(&json!("treemap")));
    // a widget is drawn from the workspace, the same way twice
    let (_, d1) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("data", json!({ "id": "laws" })))).await; assert_eq!(d1["ok"], json!(true), "{d1}"); assert_eq!(d1["view"], json!("list")); assert!(d1["data"]["items"].is_array(), "a list widget gets items: {d1}");
    let (_, d2) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("data", json!({ "id": "laws" })))).await; assert_eq!(d1["data"], d2["data"], "the same declaration draws the same data");
    // declarations the validator refuses, and a custom one it accepts
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("widget_save", json!({ "widget": { "id": "ai", "title": "ai", "source": "describe", "view": "text" } })))).await; assert_eq!(v["ok"], json!(false), "{v}"); assert!(v["error"].as_str().unwrap().contains("AI call"), "{v}");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("widget_save", json!({ "widget": { "id": "x", "title": "x", "source": "git", "view": "text", "html": "<script>1</script>" } })))).await; assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap().contains("never code"));
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("widget_save", json!({ "widget": { "id": "log", "title": "Log", "source": "history", "view": "list", "args": { "limit": 5 } } })))).await; assert_eq!(v["ok"], json!(true), "{v}");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("preview", json!({ "widget": { "id": "log", "title": "Log", "source": "history", "view": "list", "args": { "limit": 5 } } })))).await; assert_eq!(v["ok"], json!(true), "a declaration is drawn before it is saved: {v}"); assert_eq!(v["view"], json!("list"));
    // the layout: place, a refused overlap saves nothing, undo, history
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("layout_place", json!({ "widget": "log", "w": 6, "h": 3 })))).await; assert_eq!(v["ok"], json!(true), "{v}"); let n = v["layout"]["n"].as_u64().unwrap();
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("layout_set", json!({ "grid": [{ "widget": "git", "x": 0, "y": 0, "w": 8, "h": 3 }, { "widget": "log", "x": 4, "y": 1, "w": 6, "h": 3 }] })))).await; assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap().contains("overlaps"));
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("layout", json!({})))).await; assert_eq!(v["n"], json!(n), "a refused change saved nothing");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("layout_undo", json!({})))).await; assert_eq!(v["ok"], json!(true), "{v}"); assert_eq!(v["layout"]["n"], json!(n + 1)); assert!(!v["layout"]["grid"].to_string().contains("\"log\""), "undo takes the widget off again");
    // the agents' door: the same store, by MCP
    let (pane, _wt) = new_chat(&app).await;
    let mcp = |tool: &str, args: Value| { let app = app.clone(); let tool = tool.to_string(); async move { let (_, v) = call(&app, "POST", "/api/v1/cockpit/cowork/mcp", Some(json!({ "pane_id": pane, "tool": tool, "args": args }))).await; v } };
    let v = mcp("antiloki_xray_layout", json!({ "op": "place", "widget": "log", "w": 4, "h": 2 })).await; assert_eq!(v["ok"], json!(true), "an agent may arrange the board: {v}");
    let pkg = json!({ "name": "health", "version": "1", "widgets": [{ "id": "files", "title": "Files", "source": "tree", "view": "list" }] });
    let v = mcp("antiloki_xray_install", json!({ "op": "install", "package": pkg })).await; assert!(v["text"].as_str().unwrap_or("").contains("only the person") || v["ok"] == json!(false), "an agent cannot install: {v}");
    let v = mcp("antiloki_xray_install", json!({ "op": "propose", "package": pkg })).await; assert_eq!(v["ok"], json!(true), "{v}");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("widgets", json!({})))).await; assert_eq!(v["proposals"][0]["name"], json!("health")); assert!(v["packages"].as_array().unwrap().is_empty(), "proposed is not installed");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("install", json!({ "proposal": "health" })))).await; assert_eq!(v["ok"], json!(true), "{v}");
    let v = mcp("antiloki_xray_preview", json!({ "id": "health.files" })).await; assert_eq!(v["ok"], json!(true), "the installed widget draws: {v}");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("view_save", json!({ "name": "morning" })))).await; assert_eq!(v["ok"], json!(true), "{v}"); let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("views", json!({})))).await; assert_eq!(v["views"][0]["name"], json!("morning"));
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("remove", json!({ "name": "health" })))).await; assert_eq!(v["ok"], json!(true), "{v}");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("layout", json!({})))).await; assert!(!v["grid"].to_string().contains("health.files"), "a removed package leaves the board");
    // SLOTS: every part of today's X-ray is replaceable by a widget and restorable, never deletable; the shape travels in a theme as YAML
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("slots", json!({})))).await; assert!(v["slots"].as_array().unwrap().len() >= 19, "{v}"); assert!(v["slots"].as_array().unwrap().iter().all(|x| x["state"] == json!("built-in")), "the default is today's X-ray");
    let v = mcp("antiloki_xray_slots", json!({ "op": "replace", "slot": "overview", "widget": "log" })).await; assert_eq!(v["ok"], json!(true), "an agent may replace a part with a declared widget: {v}");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("slots", json!({})))).await; let ov = v["slots"].as_array().unwrap().iter().find(|x| x["slot"] == json!("overview")).unwrap().clone(); assert_eq!(ov["state"], json!("replaced")); assert_eq!(ov["widget"], json!("log"));
    let v = mcp("antiloki_xray_slots", json!({ "op": "replace", "slot": "overview", "widget": "ghost" })).await; assert_eq!(v["ok"], json!(false), "a slot cannot name a widget that is not declared: {v}");
    let themes = env_once().join("themes-store"); std::fs::create_dir_all(themes.join("mine")).unwrap(); std::env::set_var("ANTILOKI_THEMES_DIR", &themes);
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("shape_export", json!({ "theme": "mine" })))).await; assert_eq!(v["ok"], json!(true), "{v}"); let y = std::fs::read_to_string(themes.join("mine/xray.yaml")).unwrap(); assert!(y.contains("xray:") && y.contains("overview: log"), "{y}");
    let v = mcp("antiloki_xray_slots", json!({ "op": "restore", "slot": "overview" })).await; assert_eq!(v["ok"], json!(true), "{v}");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("slots", json!({})))).await; assert!(v["slots"].as_array().unwrap().iter().all(|x| x["state"] == json!("built-in")), "restored: the built-in was underneath all along");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("shape_import", json!({ "theme": "mine" })))).await; assert_eq!(v["ok"], json!(true), "the person loads the theme's shape back: {v}");
    let (_, v) = call(&app, "POST", "/api/v1/xray/v2", Some(x2("slots", json!({})))).await; assert_eq!(v["slots"].as_array().unwrap().iter().find(|x| x["slot"] == json!("overview")).unwrap()["widget"], json!("log"));
}

// ── PROVIDER SWITCH AND NEW-TEAM ONBOARDING, THROUGH THEIR ENDPOINTS (issue 17: they were exercised by hand only) ──────────────────────────────────

async fn mcp_tool(app: &axum::Router, pane: i64, tool: &str, args: Value) -> Value {
    let (st, v) = call(app, "POST", "/api/v1/cockpit/cowork/mcp", Some(json!({ "pane_id": pane, "tool": tool, "args": args }))).await;
    assert_eq!(st, StatusCode::OK, "{v}"); v
}

#[tokio::test]
async fn j35_a_provider_switch_refuses_what_it_should_and_hands_the_conversation_over_when_it_goes_through() {
    let _g = serial(); let app = app();
    let (pane, _wt) = new_chat(&app).await;
    let switch = |body: Value| { let app = app.clone(); async move { call(&app, "POST", "/api/v1/cockpit/pane/provider", Some(body)).await.1 } };
    let v = switch(json!({ "pane_id": pane, "engine": "nope" })).await; assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap_or("").contains("not a provider"), "{v}");
    let v = switch(json!({ "pane_id": 999_999, "engine": "claude-code" })).await; assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap_or("").contains("no such pane"), "{v}");
    // the same provider again with no model: nothing to hand over
    let v = switch(json!({ "pane_id": pane, "engine": "claude-code" })).await; assert_eq!(v["ok"], json!(true)); assert_eq!(v["changed"], json!(false), "{v}");
    // a local model is the same provider on another brain, and the whole handover runs: the episode is closed, the pack is written, the terminal is to restart
    let v = switch(json!({ "pane_id": pane, "engine": "claude-code", "model": "local:qwen3:0.6b" })).await;
    assert_eq!(v["ok"], json!(true), "{v}"); assert_eq!(v["changed"], json!(true)); assert_eq!(v["restart"], json!(true)); assert!(v["pack_chars"].as_u64().unwrap_or(0) > 0, "the reconciliation pack has text: {v}");
}

#[tokio::test]
async fn j36_the_new_team_flow_marks_the_pane_asks_the_two_questions_and_makes_one_team_only() {
    let _g = serial(); let app = app();
    let (pane, _wt) = new_chat(&app).await; let (other, _) = new_chat(&app).await;
    let (_, v) = call(&app, "POST", "/api/v1/cockpit/team/onboard", Some(json!({ "pane_id": 999_999 }))).await; assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap_or("").contains("no such pane"), "{v}");
    // before the pane is marked, the tool is refused: only the leader of a team being set up may use it
    let v = mcp_tool(&app, pane, "antiloki_team_setup", json!({ "op": "ask" })).await; assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap_or("").contains("only the leader of a new team"), "{v}");
    let (_, v) = call(&app, "POST", "/api/v1/cockpit/team/onboard", Some(json!({ "pane_id": pane }))).await; assert_eq!(v["ok"], json!(true), "{v}");
    // the tool is routed to the onboarding handler (not the team-graph one, whose names it shares a prefix with) and asks the two questions
    let v = mcp_tool(&app, pane, "antiloki_team_setup", json!({ "op": "ask" })).await; assert_eq!(v["ok"], json!(true), "{v}"); assert!(v["text"].as_str().unwrap_or("").contains("called?"), "{v}");
    let v = mcp_tool(&app, pane, "antiloki_team_setup", json!({ "op": "create", "name": "Calc Crew" })).await; assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap_or("").contains("both answers"), "{v}");
    let v = mcp_tool(&app, pane, "antiloki_team_setup", json!({ "op": "create", "name": "Calc Crew", "purpose": "fix the bug in calc.js" })).await; assert_eq!(v["ok"], json!(true), "{v}"); assert!(v["text"].as_str().unwrap_or("").contains("is created"), "{v}");
    // the marker goes with the team: this pane cannot make a second one, and a pane that was never marked never could
    let v = mcp_tool(&app, pane, "antiloki_team_setup", json!({ "op": "create", "name": "Second", "purpose": "again" })).await; assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap_or("").contains("only the leader of a new team"), "{v}");
    let v = mcp_tool(&app, other, "antiloki_team_setup", json!({ "op": "create", "name": "Third", "purpose": "again" })).await; assert_eq!(v["ok"], json!(false), "{v}");
}

#[tokio::test]
async fn j37_an_agent_can_ask_the_serve_to_open_the_connectors_pane_and_is_told_plainly_when_no_screen_is_there() {
    let _g = serial(); let app = app();
    let (pane, _wt) = new_chat(&app).await;
    // no antiloki screen is open in a journey: the answer says so and gives the person's own way in
    let v = mcp_tool(&app, pane, "antiloki_connector", json!({ "op": "open" })).await;
    assert_eq!(v["ok"], json!(false), "{v}"); let e = v["error"].as_str().unwrap_or(""); assert!(e.contains("could not open it") && e.contains("Connectors"), "{v}");
    // an unknown connector is refused before the screen is asked
    let v = mcp_tool(&app, pane, "antiloki_connector", json!({ "op": "open", "slug": "nope" })).await; assert_eq!(v["ok"], json!(false)); assert!(v["error"].as_str().unwrap_or("").contains("no connector called"), "{v}");
    // a connector is listed, then opened by name (still no screen)
    let v = mcp_tool(&app, pane, "antiloki_connector", json!({ "op": "list" })).await; assert_eq!(v["ok"], json!(true), "{v}");
}

