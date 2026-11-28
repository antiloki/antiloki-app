//! THE FILE DESCRIBER — prose for the files that matter, priced before it is bought, on the cheapest seat that can do it.
//!
//! WHY (operator, X-ray audit 2026-09-30/10-01: "X-ray summaries empty (0% prose) still unresolved"): every file review in
//! X-ray showed its deterministic facts and no sentence about what the file is FOR. It was not broken and it was not
//! unwired — it was never asked. `codeDescribe` writes ONE essay for ONE file from a button in the atlas panel;
//! onboarding gathers the facts of every file and stops, and its `summarize` flag builds the architecture digest (one
//! call), not descriptions. A fresh onboarding is therefore 0% prose by construction, and nothing said so.
//!
//! WHAT THIS ADDS, and what it deliberately does not:
//!   · `POST /api/v1/codebase/describe/plan {budget?, origin?}` — how much of the repo has prose (the number
//!     onboarding now reports too), the first `budget` files worth a sentence (most depended-on first), the seat that
//!     would write them and what it would cost. NO model call. This is the "show the estimate first" step.
//!   · `POST /api/v1/codebase/describe/batch {budget, confirm:true}` — runs exactly that plan, two calls at a time, stops
//!     on the first quota wall (the seat's limit memory is asked after a failed call: `codeDescribe` swallows the provider's text), and says what it did. Without `confirm` it returns the plan and calls nothing. The
//!     budget is a file count (default 40, never more than 400): a 7k-file repo can never be described by accident.
//!   · `GET /api/v1/codebase/describe/status` and `POST …/cancel`.
//!
//! WHICH SEAT: the measured winner of a `describe` cell when the record has one (`merges::ranked_seats`) — none exists
//! yet, so today it is the cheapest low-tier model among the seats that can take a call right now (installed, signed in,
//! switched on, not out of quota: `cli::seat_ok`), the price being the API-equivalent list rate of that model. The five
//! seats are flat subscriptions, so the figure is a yardstick, not a bill; it is printed as `≈ estimated` for that
//! reason. At equal price the quota that is cheapest to spend goes first and Claude last: the operator's Claude budget is
//! the scarcest, and a seat that is switched off is never offered, so a box without Claude never asks for it.
//!
//! WHAT AN ESTIMATE IS HERE: tokens counted the way `estimate::tokens_of` counts them (the facts the prompt carries + the
//! prompt itself + the read-only system line), a fixed 120 tokens of answer, times the model's list price. The CLI's own
//! preamble is NOT counted — it differs per seat and version, and counting a guess would put a confident figure on
//! something unmeasured.
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use crate::cli::Engine;
use crate::dispatch::{engine_call, AppState};
use antiloki_rs::codebase_code_describe::{describe_candidates, describeCoverage, pick_for_description, DescribeCandidate};

pub const DEFAULT_BUDGET: usize = 40;
pub const MAX_BUDGET: usize = 400;
/// the answer is 2-4 sentences (`code.describe.v1` caps it at 600 tokens)
const OUT_TOKENS: i64 = 120;
/// a headless CLI call for one short answer, wall clock — a planning figure, said as such
const SECONDS_PER_CALL: f64 = 12.0;
const PARALLEL: usize = 2;
/// the facts the prompt carries are a subset of `facts_json` (24 symbols, 30 imports, 30 dependents)
const FACTS_TOKEN_CAP: i64 = 1500;
/// when the price is a tie, which seat's quota is cheapest to spend: Claude's is the scarcest
const TIE_ORDER: [Engine; 6] = [Engine::Gemini, Engine::Cursor, Engine::Grok, Engine::Codex, Engine::Opencode, Engine::ClaudeCode];

/// One seat as the describer sees it today.
#[derive(Clone, Debug, PartialEq)]
pub struct SeatRow {
    pub engine: Engine,
    pub model: Option<String>,
    pub price_out: f64,
    pub usable: bool,
    /// why a seat is not offered, in words
    pub why: String,
}

/// The seat that writes the prose: the first measured one that is usable, else the cheapest usable. `measured` is the
/// record's ranking for the job as `engine:model` keys, best first (empty until a `describe` cell is benched). Pure — tested.
pub fn choose_seat(rows: &[SeatRow], measured: &[String]) -> Option<(SeatRow, &'static str)> {
    for key in measured {
        let e = key.split(':').next().unwrap_or("");
        if let Some(r) = rows.iter().find(|r| r.usable && r.engine.key() == e) { return Some((r.clone(), "measured")); }
    }
    let rank = |e: Engine| TIE_ORDER.iter().position(|x| *x == e).unwrap_or(TIE_ORDER.len());
    let mut usable: Vec<&SeatRow> = rows.iter().filter(|r| r.usable).collect();
    usable.sort_by(|a, b| a.price_out.partial_cmp(&b.price_out).unwrap_or(std::cmp::Ordering::Equal).then(rank(a.engine).cmp(&rank(b.engine))));
    usable.first().map(|r| ((*r).clone(), "price"))
}

/// Every seat, its low-tier model, its list price and whether it can take a call right now.
pub fn seat_rows_now() -> Vec<SeatRow> {
    [Engine::ClaudeCode, Engine::Codex, Engine::Gemini, Engine::Cursor, Engine::Grok, Engine::Opencode].into_iter().map(|e| {
        let model = crate::cli::tier_model(e, crate::cli::Tier::Low);
        let blocked = crate::cli::model_blocked(e, model.as_deref());
        let why = if !crate::cli::engine_usable(e) { "not installed, not signed in, or switched off".to_string() }
            else if let Some((_, msg)) = crate::cli::seat_limit(e) { format!("out of quota: {msg}") }
            else if let Some(b) = blocked.clone() { b }
            else { String::new() };
        SeatRow { engine: e, price_out: crate::cli::price_of(e, model.as_deref()), usable: why.is_empty(), model, why }
    }).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Estimate { pub files: usize, pub tokens_in: i64, pub tokens_out: i64, pub usd: f64, pub minutes: f64 }

/// What `files` would cost on `seat`. `fixed_in` is the prompt + system text every call carries. Pure — tested.
pub fn estimate(files: &[DescribeCandidate], seat: &SeatRow, fixed_in: i64) -> Estimate {
    let tokens_in: i64 = files.iter().map(|f| fixed_in + (f.facts_chars as i64 / 5).min(FACTS_TOKEN_CAP)).sum();
    let tokens_out = OUT_TOKENS * files.len() as i64;
    let usd = crate::cli::price_tokens(seat.engine, seat.model.as_deref(), tokens_in, tokens_out, 0, 0);
    let minutes = files.len() as f64 * SECONDS_PER_CALL / PARALLEL as f64 / 60.0;
    Estimate { files: files.len(), tokens_in, tokens_out, usd, minutes }
}

fn fixed_prompt_tokens() -> i64 {
    // the description template + the read-only line `cli_completions` puts in front of every substrate call
    crate::estimate::tokens_of(antiloki_rs::codebase_code_describe::TEMPLATE_TEXT) + crate::estimate::tokens_of(&crate::prompts::text("cli.readonly_system"))
}

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn json_resp(code: StatusCode, v: Value) -> Response { (code, Json(v)).into_response() }
fn body_json(b: &Bytes) -> Value { serde_json::from_slice(b).unwrap_or_else(|_| json!({})) }

fn budget_of(b: &Value) -> usize {
    b.get("budget").and_then(|x| x.as_u64().or_else(|| x.as_str().and_then(|t| t.trim().parse().ok()))).map(|n| (n as usize).min(MAX_BUDGET)).unwrap_or(DEFAULT_BUDGET)
}

/// The whole plan as JSON, from the candidates. Pure over its arguments — tested.
pub fn plan_json(coverage: &Value, cands: &[DescribeCandidate], budget: usize, rows: &[SeatRow], measured: &[String], fixed_in: i64) -> Value {
    let picked = pick_for_description(cands, budget);
    let seats: Vec<Value> = rows.iter().map(|r| json!({ "engine": r.engine.key(), "model": r.model, "usd_per_m_out": r.price_out, "usable": r.usable, "why_not": r.why })).collect();
    let files: Vec<Value> = picked.iter().take(25).map(|c| json!({ "path": c.path, "dependents": c.dependents, "symbols": c.symbols })).collect();
    let Some((seat, source)) = choose_seat(rows, measured) else {
        return json!({ "ok": false, "coverage": coverage, "budget": budget, "files_planned": picked.len(), "files": files, "seats": seats,
            "error": "no seat can take a call right now (none installed and signed in, or every one is switched off or out of quota) - nothing was estimated" });
    };
    let e = estimate(&picked, &seat, fixed_in);
    json!({
        "ok": true, "coverage": coverage, "budget": budget, "files_planned": picked.len(), "files": files,
        "seat": { "engine": seat.engine.key(), "model": seat.model, "chosen_by": source,
                  "why": if source == "measured" { "the record's measured winner for this job" } else { "no describe cell is measured yet: the cheapest low-tier model among the seats that can answer today" } },
        "seats": seats,
        "estimate": { "basis": "estimated", "tokens_in": e.tokens_in, "tokens_out": e.tokens_out, "usd_api_equivalent": (e.usd * 1000.0).round() / 1000.0,
                      "minutes": (e.minutes * 10.0).round() / 10.0,
                      "note": "API-equivalent list price of the seat's low-tier model (the seats are flat subscriptions, so this is a yardstick, not a bill); the CLI's own preamble is not counted; the time assumes 12 s a call, two at a time" },
        "run": if picked.is_empty() { "nothing to describe within this budget".to_string() } else { format!("POST /api/v1/codebase/describe/batch {{\"budget\": {}, \"confirm\": true}} runs exactly these {} files", picked.len(), picked.len()) },
    })
}

async fn read_all(state: &Arc<AppState>, origin: &str) -> Result<(Value, Vec<DescribeCandidate>), String> {
    let conn = state.pick();
    let origin = origin.to_string();
    tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        Ok::<_, String>((describeCoverage(&c, &origin)?, describe_candidates(&c, &origin)?))
    }).await.map_err(|e| e.to_string())?
}

fn measured_keys() -> Vec<String> { crate::merges::ranked_seats("describe", "s", 3) }

fn origin_of(b: &Value) -> String { let o = s(b.get("origin")); if o.is_empty() { "atlas".to_string() } else { o } }

/// `POST /api/v1/codebase/describe/plan` — read-only.
pub async fn plan(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body);
    match read_all(&state, &origin_of(&b)).await {
        Ok((coverage, cands)) => json_resp(StatusCode::OK, plan_json(&coverage, &cands, budget_of(&b), &seat_rows_now(), &measured_keys(), fixed_prompt_tokens())),
        Err(e) => json_resp(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": { "message": e } })),
    }
}

// ── the run ───────────────────────────────────────────────────────────────────────────────────────────────────
#[derive(Clone, Debug, Default)]
pub struct Batch {
    pub id: i64,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub running: bool,
    pub cancelled: bool,
    pub stopped: Option<String>,
    pub seat: String,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub errors: Vec<String>,
}
static BATCH: Mutex<Option<Batch>> = Mutex::new(None);
fn with_batch<R>(f: impl FnOnce(&mut Batch) -> R) -> Option<R> { BATCH.lock().unwrap_or_else(|p| p.into_inner()).as_mut().map(f) }
fn snapshot() -> Option<Batch> { BATCH.lock().unwrap_or_else(|p| p.into_inner()).clone() }

/// Three failures in a row mean the seat is not answering (a dead login, a wrong model): stop and say so rather than burn
/// the rest of the budget on the same error. A quota wall stops at once. Pure — tested.
pub fn should_stop(error: &str, consecutive_failures: usize) -> Option<String> {
    if crate::cli::is_limit_error(error) { return Some(format!("the seat hit its limit: {}", error.chars().take(160).collect::<String>())); }
    if consecutive_failures >= 3 { return Some(format!("3 calls in a row failed - the seat is not answering ({})", error.chars().take(160).collect::<String>())); }
    None
}

/// WHEN THE BATCH STOPS, given what the seat's limit memory says as well (review 2026-10-01). `codeDescribe` swallows every `aiCall` error (the twin's `try { } catch { summary_md = null }`)
/// and returns Ok with no text, so the provider's own words never reach `should_stop` and `is_limit_error` could not match: a quota wall was caught only by "3 in a row". The call
/// itself did hear it - `run_cli` files a usage limit or a lapsed subscription with `note_limit` - so the batch asks the seat's memory after a failure. Pure over its arguments - tested.
pub fn stop_for(error: &str, consecutive_failures: usize, limit: Option<(i64, String)>) -> Option<String> {
    if let Some((_, why)) = limit { return Some(format!("the seat hit its limit: {}", why.chars().take(160).collect::<String>())); }
    should_stop(error, consecutive_failures)
}

fn batch_json(b: &Batch) -> Value {
    json!({ "id": b.id, "running": b.running, "cancelled": b.cancelled, "total": b.total, "done": b.done, "failed": b.failed, "skipped": b.skipped, "stopped": b.stopped,
            "seat": b.seat, "started_ms": b.started_ms, "ended_ms": b.ended_ms, "errors": b.errors })
}

/// `POST /api/v1/codebase/describe/batch {budget, confirm, origin?}`.
pub async fn batch(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body);
    let origin = origin_of(&b);
    let budget = budget_of(&b);
    let (coverage, cands) = match read_all(&state, &origin).await {
        Ok(x) => x,
        Err(e) => return json_resp(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": { "message": e } })),
    };
    let picked = pick_for_description(&cands, budget);
    let rows = seat_rows_now();
    let measured = measured_keys();
    let plan = plan_json(&coverage, &cands, budget, &rows, &measured, fixed_prompt_tokens());
    if b.get("confirm") != Some(&json!(true)) {
        let mut p = plan; p["confirm_required"] = json!(true); p["started"] = json!(false);
        return json_resp(StatusCode::OK, p);
    }
    if plan["ok"] != json!(true) { return json_resp(StatusCode::CONFLICT, plan); }
    if picked.is_empty() { let mut p = plan; p["started"] = json!(false); return json_resp(StatusCode::OK, p); }
    let (seat, _) = choose_seat(&rows, &measured).expect("plan ok means a seat");
    {
        let mut g = BATCH.lock().unwrap_or_else(|p| p.into_inner());
        if g.as_ref().map_or(false, |x| x.running) { return json_resp(StatusCode::CONFLICT, json!({ "error": { "message": "a description batch is already running" }, "batch": g.as_ref().map(batch_json) })); }
        let id = g.as_ref().map_or(1, |x| x.id + 1);
        *g = Some(Batch { id, total: picked.len(), running: true, seat: format!("{}:{}", seat.engine.key(), seat.model.clone().unwrap_or_default()), started_ms: crate::cli::now_ms(), ..Default::default() });
    }
    let paths: Vec<String> = picked.iter().map(|c| c.path.clone()).collect();
    tokio::spawn(run_batch(state, origin, paths, seat.engine));
    let mut p = plan; p["started"] = json!(true); p["batch"] = snapshot().map(|x| batch_json(&x)).unwrap_or(Value::Null);
    json_resp(StatusCode::OK, p)
}

async fn run_batch(state: Arc<AppState>, origin: String, paths: Vec<String>, engine: Engine) {
    let gate = Arc::new(tokio::sync::Semaphore::new(PARALLEL));
    let streak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for path in paths {
        let (state, origin, gate, streak) = (state.clone(), origin.clone(), gate.clone(), streak.clone());
        tasks.push(tokio::spawn(async move {
            let Ok(_permit) = gate.acquire().await else { return };
            if with_batch(|b| b.cancelled || b.stopped.is_some()).unwrap_or(true) { with_batch(|b| b.skipped += 1); return; }
            let input = json!({ "path": path, "origin": origin, "model_id": format!("cli:{}", engine.key()) });
            let out = engine_call(&state, "system.codebase.describe", input, json!({ "actor": "describe-batch", "source": "describe-batch" })).await;
            let text_ok = |v: &Value| v.get("summary_md").and_then(Value::as_str).map_or(false, |t| !t.trim().is_empty());
            match out {
                Ok((v, _)) if text_ok(&v) => { streak.store(0, std::sync::atomic::Ordering::SeqCst); with_batch(|b| b.done += 1); }
                other => {
                    let why = match other { Ok(_) => "the seat answered with no text".to_string(), Err(e) => e };
                    let limit = crate::cli::seat_limit(engine);
                    // the swallowed error text is gone, so the log carries what the seat said when it was filed
                    let why = match &limit { Some((_, msg)) => format!("{why} ({})", msg.chars().take(120).collect::<String>()), None => why };
                    let n = streak.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let stop = stop_for(&why, n, limit);
                    with_batch(|b| { b.failed += 1; if b.errors.len() < 5 { b.errors.push(format!("{path}: {}", why.chars().take(200).collect::<String>())); } if b.stopped.is_none() { b.stopped = stop; } });
                }
            }
        }));
    }
    for t in tasks { let _ = t.await; }
    with_batch(|b| { b.running = false; b.ended_ms = crate::cli::now_ms(); });
}

/// `GET /api/v1/codebase/describe/status` — the running (or last) batch and the coverage now.
pub async fn status(state: Arc<AppState>) -> Response {
    let conn = state.pick();
    let cov = tokio::task::spawn_blocking(move || { let c = conn.lock().unwrap_or_else(|p| p.into_inner()); describeCoverage(&c, "atlas") }).await.ok().and_then(|r| r.ok()).unwrap_or(Value::Null);
    json_resp(StatusCode::OK, json!({ "batch": snapshot().map(|b| batch_json(&b)), "coverage": cov }))
}

/// `POST /api/v1/codebase/describe/cancel` — files not yet started are skipped; a call in flight finishes.
pub async fn cancel() -> Response {
    let was = with_batch(|b| { let r = b.running; if r { b.cancelled = true; } r }).unwrap_or(false);
    json_resp(StatusCode::OK, json!({ "ok": true, "was_running": was, "batch": snapshot().map(|b| batch_json(&b)) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(e: Engine, model: &str, price: f64, usable: bool) -> SeatRow { SeatRow { engine: e, model: Some(model.into()), price_out: price, usable, why: if usable { String::new() } else { "off".into() } } }
    fn cand(path: &str, deps: i64, facts: usize) -> DescribeCandidate { DescribeCandidate { path: path.into(), dependents: deps, symbols: 3, role: "logic".into(), facts_chars: facts, fresh: false } }

    #[test]
    fn the_cheapest_usable_seat_writes_the_prose_and_a_switched_off_one_is_never_offered() {
        let rows = vec![row(Engine::ClaudeCode, "haiku", 5.0, true), row(Engine::Codex, "low", 10.0, true), row(Engine::Gemini, "flash", 0.28, true), row(Engine::Cursor, "composer-2.5", 1.5, true)];
        assert_eq!(choose_seat(&rows, &[]).unwrap().0.engine, Engine::Gemini);
        let no_gemini = vec![row(Engine::ClaudeCode, "haiku", 5.0, true), row(Engine::Codex, "low", 10.0, true), row(Engine::Gemini, "flash", 0.28, false), row(Engine::Cursor, "composer-2.5", 1.5, true)];
        assert_eq!(choose_seat(&no_gemini, &[]).unwrap().0.engine, Engine::Cursor, "the next cheapest");
        let no_claude = vec![row(Engine::ClaudeCode, "haiku", 5.0, false), row(Engine::Codex, "low", 10.0, true)];
        let (seat, why) = choose_seat(&no_claude, &[]).unwrap();
        assert_eq!((seat.engine, why), (Engine::Codex, "price"), "Claude is off: it is never chosen, never asked for");
        assert!(choose_seat(&[row(Engine::Codex, "low", 10.0, false)], &[]).is_none(), "nothing can answer: no seat, not a guess");
    }

    #[test]
    fn a_price_tie_goes_to_the_quota_that_is_cheapest_to_spend_and_claude_goes_last() {
        let rows = vec![row(Engine::ClaudeCode, "haiku", 1.5, true), row(Engine::Cursor, "composer-2.5", 1.5, true), row(Engine::Grok, "low", 1.5, true)];
        assert_eq!(choose_seat(&rows, &[]).unwrap().0.engine, Engine::Cursor);
        let only = vec![row(Engine::ClaudeCode, "haiku", 1.5, true), row(Engine::Grok, "low", 1.5, true)];
        assert_eq!(choose_seat(&only, &[]).unwrap().0.engine, Engine::Grok);
    }

    #[test]
    fn a_measured_winner_outranks_the_price_but_only_while_it_can_answer() {
        let rows = vec![row(Engine::ClaudeCode, "haiku", 5.0, true), row(Engine::Gemini, "flash", 0.28, true)];
        let measured = vec!["claude-code:haiku".to_string(), "gemini:flash".to_string()];
        let (seat, why) = choose_seat(&rows, &measured).unwrap();
        assert_eq!((seat.engine, why), (Engine::ClaudeCode, "measured"), "measured above claimed");
        let claude_off = vec![row(Engine::ClaudeCode, "haiku", 5.0, false), row(Engine::Gemini, "flash", 0.28, true)];
        assert_eq!(choose_seat(&claude_off, &measured).unwrap().0.engine, Engine::Gemini, "the next measured seat that can answer");
        let unmeasured_only = vec!["opencode:x".to_string()];
        assert_eq!(choose_seat(&rows, &unmeasured_only).unwrap().1, "price", "a measured seat this box lacks falls through to the price order");
    }

    #[test]
    fn a_quota_wall_stops_the_batch_at_once_even_though_codedescribe_swallows_the_error() {
        // what the live path sees: the error text is gone (`summary_md: null`), so the only trace of the wall is the seat's limit memory, filed by the call that hit it
        let none = stop_for("the seat answered with no text", 1, crate::cli::seat_limit(Engine::Gemini));
        assert_eq!(none, None, "one empty answer on a seat with no limit on file is not a stop");
        assert!(crate::cli::note_limit(Engine::Gemini, "You've hit your usage limit. Try again in 3 hours."), "the call files the wall");
        let hit = stop_for("the seat answered with no text", 1, crate::cli::seat_limit(Engine::Gemini));
        crate::cli::clear_limit(Engine::Gemini);
        assert!(hit.as_deref().is_some_and(|m| m.starts_with("the seat hit its limit: ") && m.contains("usage limit")), "{hit:?}");
        assert_eq!(stop_for("the seat answered with no text", 1, crate::cli::seat_limit(Engine::Gemini)), None, "and the memory is cleared again");
        // the old rules still stand: three in a row, and a limit spelled in the error itself
        assert!(stop_for("x", 3, None).unwrap().contains("3 calls in a row"));
        assert!(stop_for("You've hit your usage limit", 1, None).unwrap().contains("hit its limit"));
    }

    #[test]
    fn the_estimate_counts_the_facts_and_the_prompt_and_says_what_it_is() {
        let files = vec![cand("a.ts", 9, 1000), cand("b.ts", 3, 100_000), cand("c.ts", 1, 0)];
        let seat = row(Engine::Gemini, "gemini-3-flash", 0.28, true);
        let e = estimate(&files, &seat, 300);
        assert_eq!(e.files, 3);
        assert_eq!(e.tokens_in, (300 + 200) + (300 + FACTS_TOKEN_CAP) + 300, "facts are capped per file at what the prompt can carry");
        assert_eq!(e.tokens_out, 360);
        assert!(e.usd > 0.0 && e.usd < 0.01, "{e:?}");
        assert!((e.minutes - 0.3).abs() < 0.01, "3 calls x 12 s / 2 at a time = 18 s");
    }

    #[test]
    fn the_plan_shows_the_coverage_the_files_the_seat_and_the_price_before_any_call() {
        let cands = vec![cand("hub.ts", 40, 800), cand("leaf.ts", 1, 400), cand("src/a.test.ts", 50, 400)];
        let cov = json!({ "describable": 2, "with_prose": 0, "note": "0 of 2 describable files have a description (0%)." });
        let rows = vec![row(Engine::Gemini, "gemini-3-flash", 0.28, true), row(Engine::ClaudeCode, "haiku", 5.0, false)];
        let p = plan_json(&cov, &cands, 1, &rows, &[], 300);
        assert_eq!(p["ok"], json!(true));
        assert_eq!(p["files_planned"], json!(1), "the budget is a file count");
        assert_eq!(p["files"][0]["path"], json!("hub.ts"));
        assert_eq!(p["seat"]["engine"], json!("gemini"));
        assert_eq!(p["seat"]["chosen_by"], json!("price"));
        assert_eq!(p["estimate"]["basis"], json!("estimated"));
        assert!(p["estimate"]["tokens_in"].as_i64().unwrap() > 300);
        assert_eq!(p["seats"][1]["usable"], json!(false));
        assert_eq!(p["seats"][1]["why_not"], json!("off"));
        assert!(p["run"].as_str().unwrap().contains("\"confirm\": true"));
        assert_eq!(p["coverage"]["describable"], json!(2));
    }

    #[test]
    fn with_no_seat_the_plan_says_so_and_estimates_nothing() {
        let p = plan_json(&json!({}), &[cand("a.ts", 1, 10)], 5, &[row(Engine::Codex, "low", 10.0, false)], &[], 300);
        assert_eq!(p["ok"], json!(false));
        assert!(p.get("estimate").is_none());
        assert!(p["error"].as_str().unwrap().contains("no seat can take a call"));
    }

    #[test]
    fn a_quota_wall_stops_the_batch_at_once_and_three_dead_calls_stop_it_too() {
        assert!(should_stop("You've hit your usage limit. Try again at 4:44 PM", 1).unwrap().starts_with("the seat hit its limit"));
        assert_eq!(should_stop("model not found", 1), None);
        assert_eq!(should_stop("model not found", 2), None);
        assert!(should_stop("model not found", 3).unwrap().contains("not answering"));
    }

    #[test]
    fn the_budget_is_bounded_whatever_the_caller_asks() {
        assert_eq!(budget_of(&json!({})), DEFAULT_BUDGET);
        assert_eq!(budget_of(&json!({ "budget": 12 })), 12);
        assert_eq!(budget_of(&json!({ "budget": "25" })), 25);
        assert_eq!(budget_of(&json!({ "budget": 100000 })), MAX_BUDGET, "a 7k-file repo can never be described by accident");
    }
}
