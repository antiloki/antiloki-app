//! THE AUDIT (operator 2026-09-23: "every AI answer or action from the agents is audited — cost (a deterministic
//! calculator based on model vs tokens), time (a deterministic timer), tokens (a deterministic count) — feeding the
//! timeline and the benchmark, adding weights each time a model is used"). One row per call, whoever made it: a
//! goal run's planner, a fill, the verifier, the QA gate, a reader, a chat turn. Priced by `cli::price_tokens` from
//! the tokens the engine reported (the engine's own dollar figure kept beside it, never used for the sum), timed by
//! the serve's own clock, and filed with `merges::record_use` so the merge table learns from every use.
use rusqlite::params;
use serde_json::{json, Value};

/// One audited call.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Call {
    pub t: i64, pub kind: String, pub job: String, pub engine: String, pub model: String,
    pub pane_id: i64, pub run_id: i64, pub ms: i64,
    pub tokens_in: i64, pub tokens_out: i64, pub cache_read: i64, pub cache_write: i64,
    pub cost: f64, pub cost_reported: Option<f64>, pub ok: bool,
    /// WHAT THIS WORKSPACE IS BUILT IN — `gates::stack_at` over its manifests: web · node · rust · go · python
    /// · dotnet · jvm · mixed · any (PLAN-THE-LEAN-YEARS II.7, 2026-09-25). Filled by `record` from the active
    /// workspace, so no caller has to know or pass it.
    #[serde(default)] pub stack: String,
}
fn ensure(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS ai_calls (
        id INTEGER PRIMARY KEY AUTOINCREMENT, t INTEGER NOT NULL, kind TEXT NOT NULL, job TEXT NOT NULL, engine TEXT NOT NULL, model TEXT NOT NULL,
        pane_id INTEGER NOT NULL DEFAULT 0, run_id INTEGER NOT NULL DEFAULT 0, ms INTEGER NOT NULL,
        tokens_in INTEGER NOT NULL DEFAULT 0, tokens_out INTEGER NOT NULL DEFAULT 0, cache_read INTEGER NOT NULL DEFAULT 0, cache_write INTEGER NOT NULL DEFAULT 0,
        cost REAL NOT NULL DEFAULT 0, cost_reported REAL, ok INTEGER NOT NULL DEFAULT 1);
        CREATE INDEX IF NOT EXISTS ai_calls_t ON ai_calls (t);")?;
    // ADDITIVE, AND DELIBERATELY NOT ON merge_scores (2026-09-25). The technology axis belongs on the RAW call
    // log, not on the router's aggregated evidence: merge_scores is keyed on (job, size, domain, planner,
    // worker, qa), and adding a seventh key would re-split 158 already-thin rows and require rewriting six
    // statements in the path the router depends on. PLAN-THE-LEAN-YEARS II.4 says observational data is a
    // PRIOR and must not be poured into the same number as controlled benchmark data — so it accumulates here,
    // per call, where it costs nothing and can be aggregated once the sampler that will use it exists.
    if !c.prepare("SELECT 1 FROM pragma_table_info('ai_calls') WHERE name = 'stack'")?.exists([])? {
        c.execute_batch("ALTER TABLE ai_calls ADD COLUMN stack TEXT NOT NULL DEFAULT ''")?;
    }
    Ok(())
}

/// THE ACTIVE WORKSPACE'S ROOT, told to this module rather than looked up from it.
///
/// A call path must not open the state database to log a call — `getWorkspaceRoot` needs a connection, and
/// taking one from inside `record` is how the single-connection deadlock this codebase already has a note
/// about gets recreated. The root is pushed in when it is known and changes rarely; until something pushes
/// one, a call records no technology rather than a wrong one.
///
/// A first version read an `ANTILOKI_WORKSPACE_ROOT` environment variable, which nothing in this codebase
/// sets — the column would have shipped and stayed empty forever, which is worse than absent because it looks
/// like an answer.
static ROOT: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Called when the serve learns which workspace is active — at startup and on every activation.
pub fn set_workspace_root(root: &str) {
    if let Ok(mut g) = ROOT.write() { if *g != root { *g = root.to_string(); } }
}

/// THE ACTIVE WORKSPACE'S ROOT, for callers that need to COPY it rather than log against it — `raw_compare` makes a
/// box of it so the raw arm works on its own tree instead of on top of the answer it is being compared against.
pub fn workspace_root() -> String { ROOT.read().ok().map(|g| g.clone()).unwrap_or_default() }

/// WHAT THE ACTIVE WORKSPACE IS BUILT IN (gates::stack_at over its manifests: rust · web · node · go · python …), "" when
/// no workspace is set or nothing in it says. Public because the benchmark screen shows it beside the numbers, where a
/// guess would be worse than a blank (merges::table_payload).
pub fn workspace_stack() -> &'static str {
    let root = ROOT.read().ok().map(|g| g.clone()).unwrap_or_default();
    if root.is_empty() { "" } else { crate::gates::stack_at(&root) }
}
/// WHAT EACH PANE HAS SPENT (operator 2026-09-24: "live panes tab on the full is like showing 0, why?").
///
/// Because it asked `session/audit`, whose totals join `brain.ai_calls.session_id` — and every one of the 612
/// calls on the operator's own workspace has that column NULL, so the query could never return a number. THIS
/// table is the serve's own audit, one row per call with the pane that made it, which is the attribution the
/// live-panes view actually needs. A pane with no rows is reported as having none rather than as having spent $0:
/// "not attributed" and "free" are different facts.
pub fn by_pane() -> Vec<Value> {
    let Ok(db) = crate::merges::open() else { return vec![] };
    if ensure(&db).is_err() { return vec![] }
    let Ok(mut st) = db.prepare(
        "SELECT pane_id, COUNT(*), COALESCE(SUM(tokens_in),0), COALESCE(SUM(tokens_out),0),
                COALESCE(SUM(cache_read),0), COALESCE(SUM(cost),0), COALESCE(SUM(ms),0),
                SUM(CASE WHEN ok = 0 THEN 1 ELSE 0 END), MAX(t),
                GROUP_CONCAT(DISTINCT engine), GROUP_CONCAT(DISTINCT job)
           FROM ai_calls WHERE pane_id > 0 GROUP BY pane_id ORDER BY SUM(cost) DESC") else { return vec![] };
    st.query_map([], |r| Ok(json!({
        "pane_id": r.get::<_, i64>(0)?, "calls": r.get::<_, i64>(1)?,
        "tokens_in": r.get::<_, i64>(2)?, "tokens_out": r.get::<_, i64>(3)?, "cache_read": r.get::<_, i64>(4)?,
        "cost": r.get::<_, f64>(5)?, "ms": r.get::<_, i64>(6)?, "failed": r.get::<_, i64>(7)?, "last": r.get::<_, i64>(8)?,
        "engines": r.get::<_, Option<String>>(9)?.unwrap_or_default().split(',').filter(|x| !x.is_empty()).map(String::from).collect::<Vec<_>>(),
        "jobs": r.get::<_, Option<String>>(10)?.unwrap_or_default().split(',').filter(|x| !x.is_empty()).map(String::from).collect::<Vec<_>>(),
    }))).map(|i| i.flatten().collect()).unwrap_or_default()
}

/// WHAT THIS SERVE SENT EACH SEAT in the last `hours` hours - the seat-limits feed for a CLI that publishes no quota window (Cursor, Gemini, Grok). Read from THIS table, not from a
/// workspace's `brain.ai_calls` (review 2026-10-01): that one is written only for calls that carry a prompt template (`cli::run_cli`), so every interactive pane and chat turn - the main
/// way those three seats are used - was missing, and a seat used only through panes read as "not running". `record` files every CLI call and every pane turn here, failed ones too (they
/// reached the seat), once per call, whatever workspace made it. A rolling window back from now, rows shaped for `fuel::own_use` (`house` is the engine key). Pure over the connection and the clock.
pub fn calls_since_hours_at(db: &rusqlite::Connection, now_ms: i64, hours: i64) -> Vec<Value> {
    if hours <= 0 || ensure(db).is_err() { return vec![] }
    let Ok(mut st) = db.prepare("SELECT engine, COUNT(*), COALESCE(SUM(tokens_in),0) + COALESCE(SUM(tokens_out),0), COALESCE(SUM(cost),0) FROM ai_calls WHERE t >= ?1 GROUP BY engine") else { return vec![] };
    st.query_map(params![now_ms - hours * 3_600_000], |r| { let engine: String = r.get(0)?; Ok(json!({ "model": engine.clone(), "house": engine, "calls": r.get::<_, i64>(1)?, "tokens": r.get::<_, i64>(2)?, "cost": r.get::<_, f64>(3)? })) })
        .map(|i| i.flatten().collect()).unwrap_or_default()
}
/// Every call one run made: `(job, tokens_in, tokens_out, usd)`. Pure over the connection.
pub fn run_calls_at(db: &rusqlite::Connection, run_id: i64) -> Vec<(String, i64, i64, f64)> {
    if run_id <= 0 || ensure(db).is_err() { return vec![] }
    let Ok(mut st) = db.prepare("SELECT job, tokens_in, tokens_out, cost FROM ai_calls WHERE run_id = ?1") else { return vec![] };
    st.query_map(params![run_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, f64>(3)?))).map(|i| i.flatten().collect()).unwrap_or_default()
}
pub fn run_calls(run_id: i64) -> Vec<(String, i64, i64, f64)> { match crate::merges::open() { Ok(db) => run_calls_at(&db, run_id), Err(_) => vec![] } }
/// What one goal run spent, per job: `(job, calls, usd, ms)`. Exact since `ai_calls.run_id` is filled by the job runners (2026-10-09). Pure over the connection.
pub fn calls_of_run_at(db: &rusqlite::Connection, run_id: i64) -> Vec<(String, i64, f64, i64)> {
    if run_id <= 0 || ensure(db).is_err() { return vec![] }
    let Ok(mut st) = db.prepare("SELECT job, COUNT(*), COALESCE(SUM(cost),0), COALESCE(SUM(ms),0) FROM ai_calls WHERE run_id = ?1 GROUP BY job ORDER BY 3 DESC") else { return vec![] };
    st.query_map(params![run_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?))).map(|i| i.flatten().collect()).unwrap_or_default()
}
pub fn calls_since_hours(hours: i64) -> Vec<Value> {
    match crate::merges::open() { Ok(db) => calls_since_hours_at(&db, crate::cli::now_ms(), hours), Err(_) => vec![] }
}

/// The job a prompt template names — the same names the jobs table uses. Pure — tested.
pub fn job_of_template(template: Option<&str>) -> &'static str {
    match template.unwrap_or("") {
        "goal.router_plan" | "goal.contract" | "goal.plan" => "planner",
        "goal.verify" => "verifier", "goal.task" => "editor-l", "goal.tests" => "test-writer",
        "goal.claim_check" | "goal.qa_review" | "goal.qa_evidence_plan" | "goal.qa_api_plan" | "design.judge" | "design.critique" => "qa-gate", "goal.join" | "goal.read_results" | "chat.episode_name" => "report",
        "goal.screen" => "screening", "" => "call", _ => "call",
    }
}
/// HOW MANY CALLS THIS MACHINE HAS MEASURED, AND THEIR API-EQUIVALENT COST (operator 2026-10-11, Windows: the switcher's money chip was blank after the first-run check - "all registered and logged AIs should
/// make a small cheap test to take their numbers"). Every call through the runner is a row here, the check's tiny ones included, so the chip can say "measured, nothing saved yet" instead of nothing. Never fails.
pub fn totals() -> (i64, f64) {
    let Ok(db) = crate::merges::open() else { return (0, 0.0) };
    if ensure(&db).is_err() { return (0, 0.0) }
    db.query_row("SELECT COUNT(*), COALESCE(SUM(cost), 0) FROM ai_calls", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))).unwrap_or((0, 0.0))
}
/// Record a call; returns the row's deterministic cost. Never panics, never blocks the caller on failure.
pub fn record(c: &Call) -> f64 {
    // THE COLUMN WAS NEVER WRITTEN (found 2026-09-25, the evening it shipped). `stack` was added to `ai_calls` that
    // morning with a comment saying `record` fills it from the active workspace — and the INSERT did not list it, so
    // all 180 rows on this machine carried "". A column that looks like an answer and is always empty is worse than no
    // column, because every query over it reports "unknown" and nobody suspects the writer.
    //
    // A caller that knows its own technology keeps it (the raw-compare bench runs in a box of another repo); everyone
    // else gets the active workspace's, which is what "this call happened here" means.
    let stack = if c.stack.trim().is_empty() { workspace_stack().to_string() } else { c.stack.clone() };
    if let Ok(db) = crate::merges::open() {
        if ensure(&db).is_ok() {
            let _ = db.execute("INSERT INTO ai_calls (t, kind, job, engine, model, pane_id, run_id, ms, tokens_in, tokens_out, cache_read, cache_write, cost, cost_reported, ok, stack) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![c.t, c.kind, c.job, c.engine, c.model, c.pane_id, c.run_id, c.ms, c.tokens_in, c.tokens_out, c.cache_read, c.cache_write, c.cost, c.cost_reported, c.ok as i64, stack]);
            let _ = taxonomy_note(&db, &c.job, &c.kind);
        }
    }
    // …AND THE MERGE RECORD LEARNS THE LANGUAGE TOO (II.2). Until now every real call filed `stack = any`, so the
    // language cells the polyglot bench fills could never accumulate traffic from actual work: a Rust repo's runs were
    // pooled with everything else, and the sampler read the Rust cell as "nobody routes here".
    let _ = crate::merges::record_use(&c.job, &c.engine, if c.model.is_empty() { None } else { Some(&c.model) }, c.ok, c.ms, c.cost, &stack);
    c.cost
}

/// THE TAXONOMY GROWS FROM REAL USE (PLAN-THE-LEAN-YEARS II.5: "new task types it cannot place go into a
/// `task_taxonomy` table … for the operator to name later. The taxonomy grows from real use instead of being guessed
/// up front"). One row per job the classifier has actually produced, with when it was first and last seen and how
/// often — so the question "what kinds of ask does this operator really make?" has an answer from the record rather
/// than from the enum someone typed in March. Costs nothing: it counts a verdict that already happened.
fn taxonomy_note(db: &rusqlite::Connection, job: &str, kind: &str) -> rusqlite::Result<()> {
    // THE TABLE FIRST, THE EARLY RETURN SECOND. With these two lines the other way round, `taxonomy()` — which calls
    // this with an empty job precisely to create the table — returned "no such table" on a machine that had not yet
    // recorded a turn. Found by probing the live endpoint rather than by a test, which is why the probe is worth doing.
    db.execute_batch("CREATE TABLE IF NOT EXISTS task_taxonomy (
        job TEXT NOT NULL, kind TEXT NOT NULL, n INTEGER NOT NULL DEFAULT 0,
        first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL, UNIQUE (job, kind));")?;
    if job.trim().is_empty() { return Ok(()) }
    let now = crate::cli::now_ms();
    db.execute("INSERT INTO task_taxonomy (job, kind, n, first_seen, last_seen) VALUES (?1, ?2, 1, ?3, ?3)
                ON CONFLICT(job, kind) DO UPDATE SET n = n + 1, last_seen = ?3",
        params![job, kind, now])?;
    Ok(())
}

/// What the classifier has actually seen — for the Audit screen and for `doctor`.
pub fn taxonomy() -> Result<Value, String> {
    let db = crate::merges::open().map_err(|e| e.to_string())?;
    let _ = taxonomy_note(&db, "", "");   // creates the table when nothing has been recorded yet
    let mut st = db.prepare("SELECT job, kind, n, first_seen, last_seen FROM task_taxonomy ORDER BY n DESC").map_err(|e| e.to_string())?;
    let rows: Vec<Value> = st.query_map([], |r| Ok(json!({ "job": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?,
        "n": r.get::<_, i64>(2)?, "first_seen": r.get::<_, i64>(3)?, "last_seen": r.get::<_, i64>(4)? })))
        .map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    Ok(json!({ "ok": true, "taxonomy": rows }))
}
/// A CLI call, from its trace (cli::run_cli calls this on every finish).
pub fn from_trace(t: &crate::cli::CliTrace, model: Option<&str>, template: Option<&str>) -> f64 { from_trace_tiered(t, model, None, template, 0) }
/// The same, told the TIER the call ran at (2026-09-23): a call that named no model — the blueprint's test writer, any
/// tier-only role — was filed as `<engine>:default`, which can never be a winner. The tier's model names it instead.
pub fn from_trace_tiered(t: &crate::cli::CliTrace, model: Option<&str>, tier: Option<crate::cli::Tier>, template: Option<&str>, run_id: i64) -> f64 {
    let model = model.map(String::from)
        .or_else(|| tier.and_then(|tr| crate::cli::tier_model(t.engine, tr)))
        .or_else(|| crate::cli::model_for_untiered(t.engine))
        .unwrap_or_default();
    let cost = crate::cli::price_tokens(t.engine, if model.is_empty() { None } else { Some(&model) }, t.tokens_in.unwrap_or(0), t.tokens_out.unwrap_or(0), 0, 0);
    record(&Call { t: t.ended, stack: String::new(), kind: "cli".into(), job: job_of_template(template).into(), engine: t.engine.key().into(), model, pane_id: 0, run_id, ms: t.ms,
        tokens_in: t.tokens_in.unwrap_or(0), tokens_out: t.tokens_out.unwrap_or(0), cache_read: 0, cache_write: 0, cost,
        // WHAT THE VENDOR SAID, NOT WHAT WE WORKED OUT (audit 2026-09-28). This stored `t.cost`, which for a flat
        // seat is now `priced_from_tokens` — the same table `cost` above is computed from — so the drift check
        // this column exists for compared a number with itself for exactly the seats that report nothing.
        cost_reported: t.reported, ok: t.ok })
}
/// A pane's turn (agent.rs calls this when a turn's result lands).
/// A pane's turn, TOLD WHAT THE OPERATOR ASKED FOR (II.5, 2026-09-25). Every chat turn used to be filed as the single
/// job `chat`: 25 of them on this machine, indistinguishable, while `cli::classify_ask` — the router's own
/// deterministic verdict, costing nothing — sat unused on the same text. The plan's own words: "the classification
/// already happens; it just is not recorded against the outcome." An empty ask stays `chat`, because a turn with no
/// question is not a lookup.
pub fn from_turn_asked(pane_id: i64, engine: &str, model: &str, started_ms: i64, tokens_in: i64, tokens_out: i64, cache_read: i64, cache_write: i64, ok: bool, ask: &str) -> Call {
    let job = if ask.trim().is_empty() { "chat".to_string() } else { crate::cli::classify_ask(ask).key().to_string() };
    from_turn_job(pane_id, engine, model, started_ms, tokens_in, tokens_out, cache_read, cache_write, ok, &job)
}
pub fn from_turn(pane_id: i64, engine: &str, model: &str, started_ms: i64, tokens_in: i64, tokens_out: i64, cache_read: i64, cache_write: i64, ok: bool) -> Call {
    from_turn_job(pane_id, engine, model, started_ms, tokens_in, tokens_out, cache_read, cache_write, ok, "chat")
}
fn from_turn_job(pane_id: i64, engine: &str, model: &str, started_ms: i64, tokens_in: i64, tokens_out: i64, cache_read: i64, cache_write: i64, ok: bool, job: &str) -> Call {
    let e = crate::cli::engine_of(&json!(engine)).unwrap_or(crate::cli::Engine::ClaudeCode);
    let now = crate::cli::now_ms();
    let cost = crate::cli::price_tokens(e, if model.is_empty() { None } else { Some(model) }, tokens_in, tokens_out, cache_read, cache_write);
    let c = Call { t: now, stack: String::new(), kind: "turn".into(), job: job.into(), engine: e.key().into(), model: model.into(), pane_id, run_id: 0, ms: if started_ms > 0 { now - started_ms } else { 0 },
        tokens_in, tokens_out, cache_read, cache_write, cost, cost_reported: None, ok };
    record(&c); c
}
/// The most recent calls and the totals per engine · model and per job.
pub fn report(limit: i64) -> Result<Value, String> {
    let db = crate::merges::open().map_err(|e| e.to_string())?; ensure(&db).map_err(|e| e.to_string())?;
    let mut st = db.prepare("SELECT t, kind, job, engine, model, pane_id, run_id, ms, tokens_in, tokens_out, cache_read, cache_write, cost, cost_reported, ok FROM ai_calls ORDER BY t DESC LIMIT ?1").map_err(|e| e.to_string())?;
    let rows: Vec<Value> = st.query_map(params![limit], |r| Ok(json!({ "t": r.get::<_, i64>(0)?, "kind": r.get::<_, String>(1)?, "job": r.get::<_, String>(2)?, "engine": r.get::<_, String>(3)?, "model": r.get::<_, String>(4)?, "pane_id": r.get::<_, i64>(5)?, "run_id": r.get::<_, i64>(6)?, "ms": r.get::<_, i64>(7)?, "tokens_in": r.get::<_, i64>(8)?, "tokens_out": r.get::<_, i64>(9)?, "cache_read": r.get::<_, i64>(10)?, "cache_write": r.get::<_, i64>(11)?, "cost": r.get::<_, f64>(12)?, "cost_reported": r.get::<_, Option<f64>>(13)?, "ok": r.get::<_, i64>(14)? != 0 }))).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    let mut st2 = db.prepare("SELECT engine, model, count(*), sum(ms), sum(tokens_in + tokens_out + cache_read + cache_write), sum(cost), sum(coalesce(cost_reported, 0)), sum(ok) FROM ai_calls GROUP BY engine, model ORDER BY sum(cost) DESC").map_err(|e| e.to_string())?;
    let by_model: Vec<Value> = st2.query_map([], |r| Ok(json!({ "engine": r.get::<_, String>(0)?, "model": r.get::<_, String>(1)?, "calls": r.get::<_, i64>(2)?, "ms": r.get::<_, i64>(3)?, "tokens": r.get::<_, i64>(4)?, "cost": r.get::<_, f64>(5)?, "cost_reported": r.get::<_, f64>(6)?, "ok": r.get::<_, i64>(7)? }))).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    let mut st3 = db.prepare("SELECT job, count(*), sum(ms), sum(cost) FROM ai_calls GROUP BY job ORDER BY sum(cost) DESC").map_err(|e| e.to_string())?;
    let by_job: Vec<Value> = st3.query_map([], |r| Ok(json!({ "job": r.get::<_, String>(0)?, "calls": r.get::<_, i64>(1)?, "ms": r.get::<_, i64>(2)?, "cost": r.get::<_, f64>(3)? }))).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    let total: (i64, f64, i64) = db.query_row("SELECT count(*), coalesce(sum(cost), 0), coalesce(sum(ms), 0) FROM ai_calls", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "calls": rows, "by_model": by_model, "by_job": by_job, "total": { "calls": total.0, "cost": total.1, "ms": total.2 }, "file": crate::merges::path().to_string_lossy() }))
}
/// `GET /api/v1/settings/audit?limit=200`
pub async fn http_report(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let limit = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(200).clamp(1, 2000);
    match tokio::task::spawn_blocking(move || report(limit)).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

#[cfg(test)]
mod tests {

    /// THE LIMITS FEED COUNTS WHAT PANES SENT TOO (review 2026-10-01). The first reader took `brain.ai_calls`, which only template-bound calls reach; pane and chat turns are filed here.
    #[test]
    fn the_seat_with_no_quota_feed_is_counted_from_every_call_the_serve_made_panes_included() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        ensure(&db).unwrap();
        let now = 1_800_000_000_000_i64;
        let h = 3_600_000_i64;
        let ins = |t: i64, kind: &str, engine: &str, tin: i64, tout: i64, ok: i64, pane: i64| db.execute(
            "INSERT INTO ai_calls (t, kind, job, engine, model, pane_id, ms, tokens_in, tokens_out, ok) VALUES (?1, ?2, 'chat', ?3, 'm', ?4, 1, ?5, ?6, ?7)", params![t, kind, engine, pane, tin, tout, ok]).unwrap();
        ins(now - h, "turn", "cursor", 100, 20, 1, 7);            // a pane turn: never in brain.ai_calls
        ins(now - 2 * h, "turn", "cursor", 50, 5, 0, 7);          // a failed one reached the seat too
        ins(now - 30 * h, "cli", "cursor", 1000, 100, 1, 0);      // a template-bound call, a day and a bit ago
        ins(now - h, "turn", "gemini", 7, 3, 1, 8);
        ins(now - 9 * 24 * h, "turn", "grok", 9, 9, 1, 9);
        let sum = |rows: &[Value], house: &str, k: &str| rows.iter().filter(|r| r["house"] == house).map(|r| r[k].as_i64().unwrap()).sum::<i64>();
        let h5 = calls_since_hours_at(&db, now, 5);
        assert_eq!((sum(&h5, "cursor", "calls"), sum(&h5, "cursor", "tokens")), (2, 175), "both pane turns count, the failed one too");
        assert_eq!(sum(&h5, "gemini", "calls"), 1, "a seat used only through panes is not 'not running'");
        let d7 = calls_since_hours_at(&db, now, 7 * 24);
        assert_eq!((sum(&d7, "cursor", "calls"), sum(&d7, "cursor", "tokens")), (3, 1275));
        assert_eq!(sum(&d7, "grok", "calls"), 0, "9 days ago is outside the week");
        assert!(calls_since_hours_at(&db, now, 0).is_empty());
        // and the row the fuel screen reads is built from exactly these
        let own = crate::fuel::own_use(&h5, &d7);
        assert_eq!(own.iter().find(|r| r["house"] == "gemini").map(|r| (r["calls_5h"].clone(), r["calls_7d"].clone())), Some((json!(1.0), json!(1.0))));
        assert!(own.iter().all(|r| r["house"] != "grok"), "no call in the week: no row");
    }

    /// A scratch directory of our own — this crate carries no tempfile dev-dependency, and adding one for a
    /// single test is a dependency the shipped binary would also have to resolve.
    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-audit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// THE COLUMN THAT WAS NEVER WRITTEN (2026-09-25). `stack` shipped on `ai_calls` in the morning and every one of
    /// the 180 rows on this machine carried "" by the evening, because the INSERT did not list it. This test is the
    /// one that would have caught it: it reads the row back rather than trusting the struct that went in.
    #[test]
    fn the_stack_reaches_the_row_and_is_not_merely_carried_on_the_struct() {
        let _ = std::fs::remove_file(crate::merges::path());
        let d = scratch("stack-written");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        set_workspace_root(d.to_str().unwrap());
        record(&Call { t: 1, kind: "turn".into(), stack: String::new(), job: "editor-s".into(), engine: "claude-code".into(),
            model: "sonnet".into(), pane_id: 1, run_id: 0, ms: 10, tokens_in: 1, tokens_out: 1, cache_read: 0,
            cache_write: 0, cost: 0.01, cost_reported: None, ok: true });
        let db = crate::merges::open().unwrap();
        let got: String = db.query_row("SELECT stack FROM ai_calls ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(got, "rust", "the call must be filed under the technology it was made in");
        // …and the merge record learns it too, or the language cells can never gather traffic from real work
        let m: String = db.query_row("SELECT stack FROM merge_scores ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(m, "rust");
        set_workspace_root("");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// II.5, and it costs nothing: the verdict `classify_ask` already produces, written against the outcome.
    #[test]
    fn a_turn_is_filed_under_the_job_the_operator_asked_for_not_the_bucket_chat() {
        let _ = std::fs::remove_file(crate::merges::path());
        let q = from_turn_asked(1, "claude-code", "sonnet", 0, 10, 10, 0, 0, true, "where is the workspace root read?");
        assert_eq!(q.job, "lookup", "a question is a reader's job");
        let e = from_turn_asked(1, "claude-code", "sonnet", 0, 10, 10, 0, 0, true, "in serve/src/eta.rs fix the off-by-one");
        assert_eq!(e.job, "editor-s");
        // AN EMPTY ASK IS NOT A LOOKUP. A turn with no question keeps the old bucket rather than being invented into one.
        assert_eq!(from_turn_asked(1, "claude-code", "sonnet", 0, 10, 10, 0, 0, true, "   ").job, "chat");
        // the taxonomy counts what actually arrived, so the kinds of work come from the record and not from the enum
        let t = taxonomy().unwrap();
        let jobs: Vec<String> = t["taxonomy"].as_array().unwrap().iter().map(|r| r["job"].as_str().unwrap_or("").to_string()).collect();
        assert!(jobs.contains(&"lookup".to_string()) && jobs.contains(&"editor-s".to_string()), "{jobs:?}");
        let n: i64 = t["taxonomy"].as_array().unwrap().iter().filter(|r| r["job"] == "lookup").map(|r| r["n"].as_i64().unwrap_or(0)).sum();
        assert_eq!(n, 1);
    }

    /// FOUND BY PROBING THE LIVE ENDPOINT (2026-09-26): on a machine that had recorded no turn yet, `taxonomy()`
    /// answered "no such table" — it creates the table by calling the writer with an empty job, and the writer
    /// returned before creating anything. An empty record must answer with an empty list.
    #[test]
    fn the_taxonomy_answers_an_empty_list_before_anything_has_been_classified() {
        let _ = std::fs::remove_file(crate::merges::path());
        let t = taxonomy().expect("an empty taxonomy is an answer, not an error");
        assert_eq!(t["taxonomy"].as_array().map(|a| a.len()), Some(0));
    }

    #[test]
    fn a_call_records_the_technology_of_the_workspace_it_was_made_in() {
        // nothing has told this module a root: a call claims no technology rather than guessing one
        set_workspace_root("");
        assert_eq!(workspace_stack(), "", "an unknown workspace must not be labelled");

        // a real directory: the stack comes from its own manifests, via gates::stack_at
        let d = scratch("rust-ws");
        std::fs::write(d.join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        set_workspace_root(d.to_str().unwrap());
        assert_eq!(workspace_stack(), "rust");

        // and it follows the operator when they switch workspaces
        let d2 = scratch("web-ws");
        std::fs::write(d2.join("package.json"), r#"{"dependencies":{"react":"^19"}}"#).unwrap();
        set_workspace_root(d2.to_str().unwrap());
        assert_eq!(workspace_stack(), "web");
        set_workspace_root("");
        let _ = std::fs::remove_dir_all(&d); let _ = std::fs::remove_dir_all(&d2);
    }

    /// THE MIGRATION RUNS ON A DATABASE THAT ALREADY HAS ROWS (2026-09-25). `ensure` fires on the first call
    /// after an upgrade, not at startup, so this is the only place it gets exercised before a real operator's
    /// 180 rows meet it.
    #[test]
    fn the_stack_column_is_added_without_losing_what_is_already_recorded() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        // the table exactly as it shipped before today, with a row in it
        c.execute_batch("CREATE TABLE ai_calls (
            id INTEGER PRIMARY KEY AUTOINCREMENT, t INTEGER NOT NULL, kind TEXT NOT NULL, job TEXT NOT NULL, engine TEXT NOT NULL, model TEXT NOT NULL,
            pane_id INTEGER NOT NULL DEFAULT 0, run_id INTEGER NOT NULL DEFAULT 0, ms INTEGER NOT NULL,
            tokens_in INTEGER NOT NULL DEFAULT 0, tokens_out INTEGER NOT NULL DEFAULT 0, cache_read INTEGER NOT NULL DEFAULT 0, cache_write INTEGER NOT NULL DEFAULT 0,
            cost REAL NOT NULL DEFAULT 0, cost_reported REAL, ok INTEGER NOT NULL DEFAULT 1);").unwrap();
        c.execute("INSERT INTO ai_calls (t, kind, job, engine, model, ms, cost) VALUES (1,'cli','planner','claude-code','opus',100,0.5)", []).unwrap();

        ensure(&c).unwrap();

        let has: bool = c.prepare("SELECT 1 FROM pragma_table_info('ai_calls') WHERE name='stack'").unwrap().exists([]).unwrap();
        assert!(has, "the column was not added");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM ai_calls", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "the existing row did not survive");
        // an old row claims no technology rather than claiming the wrong one
        let stack: String = c.query_row("SELECT stack FROM ai_calls WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(stack, "", "a call recorded before the column existed must not be labelled retroactively");

        // and running it twice is not an error — every call takes this path
        ensure(&c).unwrap();
        ensure(&c).unwrap();
        let cols: i64 = c.query_row("SELECT COUNT(*) FROM pragma_table_info('ai_calls') WHERE name='stack'", [], |r| r.get(0)).unwrap();
        assert_eq!(cols, 1, "the column was added more than once");
    }
    use super::*;
    #[test]
    fn a_call_is_priced_timed_counted_and_feeds_the_merges() {
        let c = Call { t: 1, kind: "cli".into(), stack: "rust".into(), job: "verifier".into(), engine: "claude-code".into(), model: "sonnet".into(), pane_id: 0, run_id: 0, ms: 4_200, tokens_in: 20_000, tokens_out: 1_000, cache_read: 0, cache_write: 0, cost: crate::cli::price_tokens(crate::cli::Engine::ClaudeCode, Some("sonnet"), 20_000, 1_000, 0, 0), cost_reported: Some(0.09), ok: true };
        assert!((record(&c) - 0.075).abs() < 1e-9, "20k in at $3 + 1k out at $15 = $0.075");
        let r = report(10).unwrap();
        assert_eq!(r["total"]["calls"], 1); assert_eq!(r["calls"][0]["job"], "verifier"); assert_eq!(r["by_model"][0]["model"], "sonnet");
        assert!((r["total"]["cost"].as_f64().unwrap() - 0.075).abs() < 1e-9, "the sum is the deterministic price, not the engine's figure");
        // the merge table got the use, under the job at size `call`, the model as the planner of a deciding job
        let rows = crate::merges::table().unwrap();
        let row = rows.iter().find(|x| x["job"] == "verifier" && x["size"] == "call").unwrap();
        assert_eq!((row["planner"].as_str(), row["worker"].as_str(), row["n"].as_i64()), (Some("claude-code:sonnet"), Some("-"), Some(1)));
        // a turn: priced with its cache, timed from when it started
        let t = from_turn(7, "claude-code", "sonnet", crate::cli::now_ms() - 3_000, 1_000, 500, 100_000, 0, true);
        assert!(t.ms >= 3_000 && t.ms < 10_000); assert!((t.cost - (0.003 + 0.0075 + 0.03)).abs() < 1e-9, "{}", t.cost);
        assert_eq!(job_of_template(Some("goal.verify")), "verifier"); assert_eq!(job_of_template(Some("goal.task")), "editor-l"); assert_eq!(job_of_template(None), "call");
        // PLAN-THE-DESIGN S2: the look judge and the critique are a QA gate's kind of call - a quota-limited judge must walk the qa-gate ladder, not the "call" one
        assert_eq!(job_of_template(Some("design.judge")), "qa-gate"); assert_eq!(job_of_template(Some("design.critique")), "qa-gate");
    }
    #[test]
    fn a_runs_calls_are_summed_by_its_own_id_and_not_by_a_time_window() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        let mk = |job: &str, run_id: i64, cost: f64| Call { t: 1, kind: "cli".into(), stack: String::new(), job: job.into(), engine: "codex".into(), model: "m".into(), pane_id: 0, run_id, ms: 1000, tokens_in: 1, tokens_out: 1, cache_read: 0, cache_write: 0, cost, cost_reported: None, ok: true };
        ensure(&db).unwrap();
        for c in [mk("qa-gate", 9, 0.25), mk("qa-gate", 9, 0.10), mk("editor-l", 9, 0.05), mk("qa-gate", 10, 7.0)] {
            db.execute("INSERT INTO ai_calls (t, kind, job, engine, model, pane_id, run_id, ms, tokens_in, tokens_out, cache_read, cache_write, cost, cost_reported, ok, stack) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![c.t, c.kind, c.job, c.engine, c.model, c.pane_id, c.run_id, c.ms, c.tokens_in, c.tokens_out, c.cache_read, c.cache_write, c.cost, c.cost_reported, c.ok as i64, c.stack]).unwrap();
        }
        let rows = calls_of_run_at(&db, 9);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].0.as_str(), rows[0].1), ("qa-gate", 2));
        assert!((rows[0].2 - 0.35).abs() < 1e-9, "run 10's $7 is not run 9's");
        assert!(calls_of_run_at(&db, 0).is_empty() && calls_of_run_at(&db, 404).is_empty());
    }
}
