//! Per-team call ledger (RFC 0011 §6). SQLite tables: calls, node_runs, audits, examples, notes,
//! versions, proposals, memory_fts; one `bench.sqlite` per team under `.antiloki/teams/<slug>/`.
use crate::team_nodebench::NodeRecord;
use regex::Regex;
use rusqlite::{params, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock, Mutex};
use std::time::Duration;

/// Relative name of the per-team dataset file (`team_files/<slug>/bench.sqlite`).
pub const BENCH_FILE: &str = "bench.sqlite";

static WRITE_FAILURES: AtomicU64 = AtomicU64::new(0);
static WRITER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
static QUEUE_LEN: Mutex<usize> = Mutex::new(0);
#[cfg(test)]
std::thread_local! {
    static FORCE_FAIL_WRITES: std::cell::RefCell<bool> = const { std::cell::RefCell::new(false) };
}

#[cfg(test)]
pub fn test_force_fail_writes(on: bool) {
    FORCE_FAIL_WRITES.with(|f| *f.borrow_mut() = on);
}

fn force_fail_writes_active() -> bool {
    #[cfg(test)]
    {
        FORCE_FAIL_WRITES.with(|f| *f.borrow())
    }
    #[cfg(not(test))]
    {
        false
    }
}

#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub root: String,
    pub team: String,
    pub run: Option<String>,
    pub node: Option<String>,
    pub pane: Option<String>,
    pub caller: String,
    pub task_class: Option<String>,
    pub cfg: Option<String>,
    pub bench: bool,
    pub try_n: Option<i32>,
    pub climbed_from: Option<String>,
    /// JSON list of the memory example ids this call's prompt was filled with (`examples_from`)
    pub examples_used: Option<String>,
    /// the router's choice for this call, as JSON (the seat, why, the seed, the evidence it drew on)
    pub route: Option<String>,
}

impl Ctx {
    /// A call outside any team graph (a goal worker, the front, frames, the substrate): it lands in the workspace's own dataset, `_workspace`.
    pub fn workspace(root: &str, caller: &str) -> Ctx {
        Ctx { root: root.into(), team: WORKSPACE_TEAM.into(), caller: caller.into(), ..Default::default() }
    }
}

/// The dataset for calls that belong to no team.
pub const WORKSPACE_TEAM: &str = "_workspace";

#[derive(Clone)]
struct CallRow {
    ctx: Ctx,
    ts_ms: i64,
    source: &'static str,
    engine: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    local: i32,
    tokens_in: Option<i64>,
    tokens_out: Option<i64>,
    cost_usd: Option<f64>,
    wall_ms: i64,
    outcome: String,
    try_n: Option<i32>,
    climbed_from: Option<String>,
    prompt: Option<String>,
    answer: Option<String>,
    prompt_hash: String,
    answer_hash: String,
    /// the seat as the grammar spells it (`endpoint:a:m`, `local:tag`, `claude-code:m`): what the router counts ok and fail by
    seat: Option<String>,
}

enum Job {
    Call(CallRow),
    NodeRuns { root: String, team: String, rows: Vec<NodeRecord> },
    ImportJsonl { root: String, team: String, path: PathBuf },
    Retention { root: String, team: String },
    Verdict { ctx: Ctx, prompt_hash: String, answer_hash: String, verified: Option<bool>, by: Option<String>, outcome: Option<String> },
    Consolidate { root: String, team: String },
}

fn teams_slug(team: &str) -> String { crate::team_graph::slug(team) }

/// Where a dataset lives: a team's folder, or `<root>/.antiloki/` for the calls that belong to no team (never inside `teams/`, which lists teams).
fn data_dir(root: &str, team: &str) -> PathBuf {
    if team == WORKSPACE_TEAM { Path::new(root).join(".antiloki") } else { Path::new(root).join(".antiloki").join("teams").join(teams_slug(team)) }
}

pub fn bench_path(root: &str, team: &str) -> PathBuf { data_dir(root, team).join(BENCH_FILE) }

const SCHEMA: &str = "
PRAGMA journal_mode=WAL;
PRAGMA busy_timeout=5000;
CREATE TABLE IF NOT EXISTS calls (
  id INTEGER PRIMARY KEY, ts_ms INTEGER NOT NULL, run TEXT, team TEXT NOT NULL, node TEXT, pane TEXT,
  caller TEXT NOT NULL,
  source TEXT NOT NULL,
  engine TEXT, provider TEXT, model TEXT, local INTEGER NOT NULL DEFAULT 0,
  cfg TEXT, task_class TEXT,
  tokens_in INTEGER, tokens_out INTEGER, cost_usd REAL, wall_ms INTEGER, first_token_ms INTEGER, tps REAL, ctx INTEGER,
  outcome TEXT NOT NULL,
  try INTEGER, climbed_from TEXT, bench INTEGER NOT NULL DEFAULT 0,
  prompt TEXT, answer TEXT, prompt_hash TEXT, answer_hash TEXT,
  verified INTEGER, verified_by TEXT, examples_used TEXT, seat TEXT, route TEXT
);
CREATE INDEX IF NOT EXISTS calls_node ON calls(node, cfg, ts_ms);
CREATE TABLE IF NOT EXISTS node_runs (run TEXT, t_ms INTEGER, node TEXT, kind TEXT, title TEXT, cfg TEXT, seat TEXT,
  tries INTEGER, ok INTEGER, outcome TEXT, cost_usd REAL, ms INTEGER, escalated INTEGER, detail TEXT,
  PRIMARY KEY (run, node));
CREATE TABLE IF NOT EXISTS audits (id INTEGER PRIMARY KEY, run TEXT, ts_ms INTEGER, good INTEGER, findings TEXT, notified INTEGER);
CREATE TABLE IF NOT EXISTS examples (id INTEGER PRIMARY KEY, cell TEXT, task_class TEXT, input TEXT, output TEXT,
  kind TEXT NOT NULL, label TEXT, source_call INTEGER, verified_by TEXT, added_ms INTEGER,
  split TEXT NOT NULL DEFAULT 'none', hash TEXT UNIQUE);
CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, ts_ms INTEGER, kind TEXT, text TEXT, links TEXT);
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(text, kind, task_class, content='');
CREATE TABLE IF NOT EXISTS versions (id INTEGER PRIMARY KEY, ts_ms INTEGER, parent INTEGER, graph TEXT, graph_hash TEXT, reason TEXT, proposal INTEGER, applied_by TEXT);
CREATE TABLE IF NOT EXISTS proposals (id INTEGER PRIMARY KEY, ts_ms INTEGER, author TEXT, kind TEXT, patch TEXT, hypothesis TEXT, metric TEXT,
  expected TEXT, evidence TEXT, risk TEXT, status TEXT, decided_by TEXT, decided_ms INTEGER, result TEXT);
";

pub fn write_failures() -> u64 { WRITE_FAILURES.load(Ordering::Relaxed) }

fn retention_days() -> i64 {
    std::env::var("ANTILOKI_LEDGER_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(90)
}

fn text_hash(s: &str) -> String { hex::encode(Sha256::digest(s.as_bytes())) }

/// Secrets replaced before text is stored (cards are handled apart: they need the Luhn check, or every long number would go).
fn sanitize_rules() -> &'static Vec<(&'static str, Regex)> {
    static RULES: std::sync::OnceLock<Vec<(&'static str, Regex)>> = std::sync::OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            ("[REDACTED_PRIVATE_KEY]", Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)").unwrap()),
            ("[REDACTED_API_KEY]", Regex::new(r"(?i)\bsk-[A-Za-z0-9_\-]{20,}").unwrap()),
            ("[REDACTED_API_KEY]", Regex::new(r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_\-]{30,}|xox[abprs]-[A-Za-z0-9\-]{10,})\b").unwrap()),
            ("authorization: bearer [REDACTED_TOKEN]", Regex::new(r"(?i)authorization:\s*bearer\s+[A-Za-z0-9._\-]+").unwrap()),
            ("bearer [REDACTED_TOKEN]", Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._\-]{6,}").unwrap()),
            ("[REDACTED_EMAIL]", Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").unwrap()),
        ]
    })
}

fn card_re() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b(?:\d[ -]?){12,18}\d\b").unwrap())
}

fn luhn(digits: &str) -> bool {
    let d: Vec<u32> = digits.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&d.len()) { return false; }
    let sum: u32 = d.iter().rev().enumerate().map(|(i, &x)| if i % 2 == 1 { let y = x * 2; if y > 9 { y - 9 } else { y } } else { x }).sum();
    sum % 10 == 0
}

/// Byte spans of payment-card numbers (13 to 19 digits that pass the Luhn check). Shared with the Check rule `pii`.
pub fn card_spans(text: &str) -> Vec<(usize, usize)> {
    card_re().find_iter(text).filter(|m| luhn(m.as_str())).map(|m| (m.start(), m.end())).collect()
}

/// Secrets and personal data replaced before text is stored.
pub fn sanitize(text: &str) -> String {
    let mut out = text.to_string();
    for (rep, re) in sanitize_rules() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    let spans = card_spans(&out);
    for (a, b) in spans.into_iter().rev() { out.replace_range(a..b, "[REDACTED_CARD]"); }
    out
}

fn ledger_off(root: &str, team: &str) -> bool {
    let dir = data_dir(root, team);
    if let Ok(t) = std::fs::read_to_string(dir.join("ledger-off.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
            if v["off"].as_bool() == Some(true) || v["ledger_off"].as_bool() == Some(true) { return true; }
        }
    }
    let gp = dir.join(format!("{}.json", teams_slug(team)));
    if let Ok(t) = std::fs::read_to_string(gp) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
            if v["ledger_off"].as_bool() == Some(true) { return true; }
        }
    }
    false
}

/// The person's switch for one team: off keeps only hashes (no prompt or answer text) from now on.
pub fn set_off(root: &str, team: &str, off: bool) -> Result<(), String> {
    let dir = data_dir(root, team);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("ledger-off.json"), json!({ "off": off }).to_string()).map_err(|e| e.to_string())
}

fn open_team_db(root: &str, team: &str) -> Result<Connection, String> {
    let p = bench_path(root, team);
    if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let c = Connection::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    c.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
    // a dataset made before `examples_used` existed gets the column once (the error is only "duplicate column")
    for col in ["examples_used", "seat", "route"] { let _ = c.execute(&format!("ALTER TABLE calls ADD COLUMN {col} TEXT"), []); }
    Ok(c)
}

fn apply_retention(c: &Connection, days: i64) -> Result<(), String> {
    let cutoff = crate::cli::now_ms() - days * 86_400_000;
    c.execute(
        "DELETE FROM calls WHERE ts_ms < ?1 AND id NOT IN (
           SELECT source_call FROM examples WHERE kind IN ('golden','regression','trap') AND source_call IS NOT NULL)",
        [cutoff],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

fn import_note_exists(c: &Connection) -> bool {
    c.query_row(
        "SELECT 1 FROM notes WHERE kind = 'import' AND text LIKE '%node-records.jsonl%' LIMIT 1",
        [],
        |_| Ok(()),
    ).is_ok()
}

fn upsert_node_run(c: &Connection, r: &NodeRecord) -> Result<(), String> {
    let detail = serde_json::to_string(&r.detail).unwrap_or_else(|_| "{}".into());
    c.execute(
        "INSERT INTO node_runs (run, t_ms, node, kind, title, cfg, seat, tries, ok, outcome, cost_usd, ms, escalated, detail)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
         ON CONFLICT(run, node) DO UPDATE SET
           t_ms=excluded.t_ms, kind=excluded.kind, title=excluded.title, cfg=excluded.cfg, seat=excluded.seat,
           tries=excluded.tries, ok=excluded.ok, outcome=excluded.outcome, cost_usd=excluded.cost_usd,
           ms=excluded.ms, escalated=excluded.escalated, detail=excluded.detail",
        params![r.run, r.t_ms, r.node, r.kind, r.title, r.cfg, r.seat, r.tries as i64, r.ok as i32, r.outcome, r.cost_usd, r.ms, r.escalated as i32, detail],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

fn do_import_jsonl(root: &str, team: &str, path: &Path) -> Result<(), String> {
    let c = open_team_db(root, team)?;
    if import_note_exists(&c) { return Ok(()); }
    let rows: Vec<NodeRecord> = std::fs::read_to_string(path).ok()
        .map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default();
    for r in &rows { upsert_node_run(&c, r)?; }
    if !rows.is_empty() {
        c.execute(
            "INSERT INTO notes (ts_ms, kind, text, links) VALUES (?1, 'import', 'node-records.jsonl imported once', ?2)",
            params![crate::cli::now_ms(), path.display().to_string()],
        ).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn insert_call(c: &Connection, row: &CallRow) -> Result<(), String> {
    let off = ledger_off(&row.ctx.root, &row.ctx.team);
    let (prompt, answer) = if off {
        (None, None)
    } else {
        (row.prompt.clone(), row.answer.clone())
    };
    c.execute(
        "INSERT INTO calls (ts_ms, run, team, node, pane, caller, source, engine, provider, model, local, cfg, task_class,
          tokens_in, tokens_out, cost_usd, wall_ms, outcome, try, climbed_from, bench, prompt, answer, prompt_hash, answer_hash, examples_used, seat, route)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28)",
        params![
            row.ts_ms,
            row.ctx.run,
            if row.ctx.team == WORKSPACE_TEAM { WORKSPACE_TEAM.to_string() } else { teams_slug(&row.ctx.team) },
            row.ctx.node,
            row.ctx.pane,
            row.ctx.caller,
            row.source,
            row.engine,
            row.provider,
            row.model,
            row.local,
            row.ctx.cfg,
            row.ctx.task_class,
            row.tokens_in,
            row.tokens_out,
            row.cost_usd,
            row.wall_ms,
            row.outcome,
            row.try_n,
            row.climbed_from,
            if row.ctx.bench { 1 } else { 0 },
            prompt,
            answer,
            row.prompt_hash,
            row.answer_hash,
            row.ctx.examples_used,
            row.seat,
            row.ctx.route,
        ],
    ).map_err(|e| e.to_string())?;
    Ok(())
}

fn apply_job(job: Job) -> Result<(), String> {
    if cfg!(test) && force_fail_writes_active() {
        return Err("forced write failure".into());
    }
    match job {
        Job::Call(row) => {
            let c = open_team_db(&row.ctx.root, &row.ctx.team)?;
            apply_retention(&c, retention_days())?;
            insert_call(&c, &row)
        }
        Job::NodeRuns { root, team, rows } => {
            let c = open_team_db(&root, &team)?;
            for r in &rows { upsert_node_run(&c, r)?; }
            Ok(())
        }
        Job::ImportJsonl { root, team, path } => do_import_jsonl(&root, &team, &path),
        Job::Retention { root, team } => {
            let c = open_team_db(&root, &team)?;
            apply_retention(&c, retention_days())
        }
        Job::Verdict { ctx, prompt_hash, answer_hash, verified, by, outcome } => {
            let c = open_team_db(&ctx.root, &ctx.team)?;
            // the newest row this node wrote for exactly this prompt and answer (the writer is one thread, so the row is already there)
            c.execute(
                "UPDATE calls SET verified = COALESCE(?1, verified), verified_by = COALESCE(?2, verified_by), outcome = COALESCE(?3, outcome)
                 WHERE id = (SELECT id FROM calls WHERE prompt_hash = ?4 AND answer_hash = ?5 AND IFNULL(run,'') = ?6 AND IFNULL(node,'') = ?7 ORDER BY id DESC LIMIT 1)",
                params![verified.map(|v| v as i32), by, outcome, prompt_hash, answer_hash, ctx.run.clone().unwrap_or_default(), ctx.node.clone().unwrap_or_default()],
            ).map_err(|e| e.to_string())?;
            Ok(())
        }
        Job::Consolidate { root, team } => {
            let c = open_team_db(&root, &team)?;
            crate::team_memory::consolidate(&c).map(|_| ())
        }
    }
}

fn writer() -> &'static mpsc::Sender<Job> {
    WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            for job in rx {
                if apply_job(job).is_err() {
                    WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
                }
                if let Ok(mut n) = QUEUE_LEN.lock() { *n = n.saturating_sub(1); }
            }
        });
        tx
    })
}

fn enqueue(job: Job) {
    if cfg!(test) {
        if apply_job(job).is_err() { WRITE_FAILURES.fetch_add(1, Ordering::Relaxed); }
        return;
    }
    if let Ok(mut n) = QUEUE_LEN.lock() { *n += 1; }
    if writer().send(job).is_err() {
        WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut n) = QUEUE_LEN.lock() { *n = n.saturating_sub(1); }
    }
}

/// Wait until queued ledger writes are applied (tests and post-audit).
pub fn flush() {
    if cfg!(test) { return; }
    for _ in 0..200 {
        let left = QUEUE_LEN.lock().map(|n| *n).unwrap_or(0);
        if left == 0 { return; }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn outcome_from_cli(result: &Result<crate::cli::RunOut, String>) -> &'static str {
    match result {
        Ok(o) => {
            if o.parsed.error.is_some() { return "refused"; }
            if o.parsed.content.trim().is_empty() { return "empty"; }
            "ok"
        }
        Err(e) => {
            let low = e.to_lowercase();
            if low.contains("time") { "timeout" }
            else if low.contains("cancel") || low.contains("killed") || low.contains("abort") { "killed" }
            else if low.contains("block") || low.contains("refus") || low.contains("policy") || low.contains("not allowed") { "refused" }
            else { "error" }
        }
    }
}

pub fn outcome_from_llm(res: &Result<(String, f64, u64), String>) -> &'static str {
    match res {
        Ok((t, _, _)) => if t.trim().is_empty() { "empty" } else { "ok" },
        Err(e) => {
            let low = e.to_lowercase();
            if low.contains("time") { "timeout" }
            else if low.contains("nothing") { "empty" }
            else if low.contains("refus") || low.contains("block") { "refused" }
            else { "error" }
        }
    }
}

/// Append one CLI call row; never fails the caller.
pub fn record_cli(
    engine: crate::cli::Engine,
    messages: &[crate::cli::ChatMessage],
    result: &Result<crate::cli::RunOut, String>,
    ctx: &Ctx,
    model: Option<String>,
) {
    let prompt_raw: String = messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n");
    let prompt_s = sanitize(&prompt_raw);
    let (answer_s, tokens_in, tokens_out, cost, wall_ms) = match result {
        Ok(o) => (
            sanitize(&o.parsed.content),
            o.parsed.tokens_in,
            o.parsed.tokens_out,
            o.parsed.cost.or(o.trace.cost),
            o.ms,
        ),
        Err(_) => (String::new(), None, None, None, 0),
    };
    let row = CallRow {
        ctx: ctx.clone(),
        ts_ms: crate::cli::now_ms(),
        source: "cli",
        engine: Some(engine.key().to_string()),
        provider: Some(engine.key().to_string()),
        model,
        local: 0,
        tokens_in,
        tokens_out,
        cost_usd: cost,
        wall_ms,
        outcome: outcome_from_cli(result).to_string(),
        try_n: ctx.try_n,
        climbed_from: ctx.climbed_from.clone(),
        prompt: Some(prompt_s.clone()),
        answer: if answer_s.is_empty() { None } else { Some(answer_s.clone()) },
        prompt_hash: text_hash(&prompt_raw),
        answer_hash: text_hash(&answer_s),
        seat: None,
    };
    enqueue(Job::Call(row));
}

/// Append one model-seat call row; never fails the caller.
pub fn record_llm(
    ctx: Option<&Ctx>,
    seat: &crate::team_llm::Seat,
    prompt: &str,
    result: &Result<(String, f64, u64), String>,
) {
    let ctx = match ctx.cloned() { Some(c) => c, None => return };
    let prompt_raw = prompt.to_string();
    let prompt_s = sanitize(&prompt_raw);
    let (source, local, engine, provider, model) = match seat {
        crate::team_llm::Seat::Endpoint { alias, model } => (
            "endpoint", 0,
            Some(format!("endpoint:{alias}")),
            Some(alias.clone()),
            model.clone().or_else(|| Some(alias.clone())),
        ),
        crate::team_llm::Seat::Local { tag } => (
            "local", 1,
            Some("local".into()),
            Some("ollama".into()),
            Some(tag.clone()),
        ),
        crate::team_llm::Seat::Claude { model } => (
            "cli", 0,
            Some("claude-code".into()),
            Some("claude-code".into()),
            Some(model.clone()),
        ),
    };
    let (answer_s, cost, wall_ms) = match result {
        Ok((t, c, ms)) => (sanitize(t), Some(*c), *ms as i64),
        Err(_) => (String::new(), Some(0.0), 0),
    };
    let answer_hash = text_hash(&answer_s);
    let (try_n, climbed_from) = (ctx.try_n, ctx.climbed_from.clone());
    let row = CallRow {
        ctx,
        ts_ms: crate::cli::now_ms(),
        source,
        engine,
        provider,
        model,
        local,
        // an ESTIMATE (about 4 characters a token): the call path returns text, cost and time only, so a local model's own count is not threaded here
        tokens_in: Some((prompt_raw.chars().count() as i64 + 3) / 4),
        tokens_out: match result { Ok((t, _, _)) => Some((t.chars().count() as i64 + 3) / 4), Err(_) => None },
        cost_usd: cost,
        wall_ms,
        outcome: outcome_from_llm(result).to_string(),
        try_n,
        climbed_from,
        prompt: Some(prompt_s.clone()),
        answer: if answer_s.is_empty() { None } else { Some(answer_s) },
        prompt_hash: text_hash(&prompt_raw),
        answer_hash,
        seat: Some(crate::team_llm::seat_key(seat)),
    };
    enqueue(Job::Call(row));
}

/// How each seat has done for one task class: seat -> (ok, fail). A call is ok when it answered and no guard caught it. This is what the router's bandit learns from.
pub fn seat_stats(root: &str, team: &str, task_class: Option<&str>) -> std::collections::BTreeMap<String, (u32, u32)> {
    flush();
    let Ok(c) = open_team_db(root, team) else { return Default::default() };
    let Ok(mut st) = c.prepare("SELECT seat, SUM(CASE WHEN outcome = 'ok' AND COALESCE(verified, 1) != 0 THEN 1 ELSE 0 END), SUM(CASE WHEN outcome = 'ok' AND COALESCE(verified, 1) != 0 THEN 0 ELSE 1 END)
        FROM calls WHERE seat IS NOT NULL AND (?1 IS NULL OR task_class = ?1 OR node = ?1) GROUP BY seat") else { return Default::default() };
    let rows = st.query_map([task_class], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)? as u32, r.get::<_, i64>(2)? as u32))));
    match rows { Ok(it) => it.flatten().collect(), Err(_) => Default::default() }
}

/// A guard (or a check, or the person) has judged one call: mark the row it wrote. `verified` Some(true) = a verified success, Some(false) = failed; `outcome` overrides the row's outcome (`guard_failed`, `parse_fail`). Never fails the caller.
pub fn mark_verdict(ctx: Option<&Ctx>, prompt: &str, answer: &str, verified: Option<bool>, by: Option<&str>, outcome: Option<&str>) {
    let Some(ctx) = ctx else { return };
    enqueue(Job::Verdict { ctx: ctx.clone(), prompt_hash: text_hash(prompt), answer_hash: text_hash(&sanitize(answer)), verified, by: by.map(String::from), outcome: outcome.map(String::from) });
}

/// Run the memory consolidation for a team after its writes (RFC 0011 §8.2).
pub fn consolidate_async(root: &str, team: &str) { enqueue(Job::Consolidate { root: root.to_string(), team: team.to_string() }); }

/// Open a team's dataset for a read or a write by another module (the memory store); the schema is made if it is not there.
pub fn with_db<T>(root: &str, team: &str, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
    flush();
    let c = open_team_db(root, team)?;
    f(&c)
}

/// Import `node-records.jsonl` once, upsert fresh rows, and keep jsonl in sync.
pub fn sync_node_records(root: &str, team: &str, jsonl_path: &Path, rows: &[NodeRecord]) {
    enqueue(Job::ImportJsonl { root: root.to_string(), team: team.to_string(), path: jsonl_path.to_path_buf() });
    if !rows.is_empty() {
        enqueue(Job::NodeRuns { root: root.to_string(), team: team.to_string(), rows: rows.to_vec() });
    }
}

#[derive(Clone, Debug)]
pub struct VersionRow {
    pub id: i64,
    pub ts_ms: i64,
    pub parent: Option<i64>,
    pub graph: String,
    pub graph_hash: String,
    pub reason: String,
    pub proposal: Option<i64>,
    pub applied_by: String,
}

impl VersionRow {
    pub fn meta_json(&self) -> serde_json::Value {
        json!({
            "id": self.id, "ts_ms": self.ts_ms, "parent": self.parent,
            "graph_hash": self.graph_hash, "reason": self.reason,
            "proposal": self.proposal, "applied_by": self.applied_by,
        })
    }
}

pub fn latest_version_id(root: &str, team: &str) -> Option<i64> {
    let c = open_team_db(root, team).ok()?;
    c.query_row("SELECT id FROM versions ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).ok()
}

pub fn write_version(
    root: &str,
    team: &str,
    parent: Option<i64>,
    graph: &crate::team_graph::Graph,
    reason: &str,
    proposal: Option<i64>,
    applied_by: &str,
) -> Result<i64, String> {
    let c = open_team_db(root, team)?;
    let body = serde_json::to_string(&graph.to_json()).map_err(|e| e.to_string())?;
    let hash = crate::team_graph_run::graph_hash(graph);
    c.execute(
        "INSERT INTO versions (ts_ms, parent, graph, graph_hash, reason, proposal, applied_by) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![crate::cli::now_ms(), parent, body, hash, reason, proposal, applied_by],
    ).map_err(|e| e.to_string())?;
    Ok(c.last_insert_rowid())
}

pub fn read_version(root: &str, team: &str, id: i64) -> Result<VersionRow, String> {
    let c = open_team_db(root, team)?;
    c.query_row(
        "SELECT id, ts_ms, parent, graph, graph_hash, reason, proposal, applied_by FROM versions WHERE id = ?1",
        [id],
        |r| Ok(VersionRow {
            id: r.get(0)?,
            ts_ms: r.get(1)?,
            parent: r.get(2)?,
            graph: r.get(3)?,
            graph_hash: r.get(4)?,
            reason: r.get(5)?,
            proposal: r.get(6)?,
            applied_by: r.get(7)?,
        }),
    ).map_err(|e| e.to_string())
}

pub fn list_versions(root: &str, team: &str) -> Result<Vec<VersionRow>, String> {
    let c = open_team_db(root, team)?;
    let mut stmt = c.prepare("SELECT id, ts_ms, parent, graph, graph_hash, reason, proposal, applied_by FROM versions ORDER BY id ASC").map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| Ok(VersionRow {
        id: r.get(0)?,
        ts_ms: r.get(1)?,
        parent: r.get(2)?,
        graph: r.get(3)?,
        graph_hash: r.get(4)?,
        reason: r.get(5)?,
        proposal: r.get(6)?,
        applied_by: r.get(7)?,
    })).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn truncate_versions(root: &str, team: &str, keep: usize) -> Result<(), String> {
    let c = open_team_db(root, team)?;
    let n: i64 = c.query_row("SELECT COUNT(*) FROM versions", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if n as usize <= keep { return Ok(()); }
    let drop = n as usize - keep;
    c.execute("DELETE FROM versions WHERE id IN (SELECT id FROM versions ORDER BY id ASC LIMIT ?1)", [drop as i64]).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn mark_proposal(root: &str, team: &str, id: i64, status: &str, decided_by: &str) -> Result<(), String> {
    let c = open_team_db(root, team)?;
    c.execute("UPDATE proposals SET status = ?1, decided_by = ?2, decided_ms = ?3 WHERE id = ?4", params![status, decided_by, crate::cli::now_ms(), id]).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn proposal_status(root: &str, team: &str, id: i64) -> Option<String> {
    let c = open_team_db(root, team).ok()?;
    c.query_row("SELECT status FROM proposals WHERE id = ?1", [id], |r| r.get(0)).ok()
}

pub fn write_proposal(root: &str, team: &str, author: &str, patch: &str, reason: &str) -> Result<i64, String> {
    let c = open_team_db(root, team)?;
    c.execute(
        "INSERT INTO proposals (ts_ms, author, kind, patch, hypothesis, status) VALUES (?1,?2,'patch',?3,?4,'pending')",
        params![crate::cli::now_ms(), author, patch, reason],
    ).map_err(|e| e.to_string())?;
    Ok(c.last_insert_rowid())
}

#[cfg(test)]
pub fn call_count(root: &str, team: &str) -> i64 {
    flush();
    let c = open_team_db(root, team).unwrap();
    c.query_row("SELECT COUNT(*) FROM calls", [], |r| r.get(0)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ChatMessage, Engine, RunOut, CliParsed, CliTrace};
    use crate::team_llm::Seat;

    static NTH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn ws() -> (PathBuf, String) {
        let n = NTH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("antiloki-ledger-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        (d.clone(), d.to_string_lossy().into_owned())
    }

    fn ctx(root: &str) -> Ctx {
        Ctx { root: root.into(), team: "bench-team".into(), run: Some("r1".into()), node: Some("n1".into()),
            pane: None, caller: "team-node".into(), task_class: None, cfg: Some("cfgabc".into()), bench: true, try_n: None, climbed_from: None, examples_used: None, route: None }
    }

    fn ok_out(content: &str, tin: i64, tout: i64, cost: f64, ms: i64) -> RunOut {
        RunOut { parsed: CliParsed { content: content.into(), tokens_in: Some(tin), tokens_out: Some(tout), cost: Some(cost), tools: vec![], error: None }, ms, trace: CliTrace::replayed(Engine::ClaudeCode) }
    }

    #[test]
    fn a_row_is_appended_per_cli_call_with_tokens_cost_and_outcome() {
        let (_d, root) = ws();
        let c = ctx(&root);
        record_cli(Engine::ClaudeCode, &[ChatMessage { role: "user".into(), content: "hi".into() }], &Ok(ok_out("hello", 10, 5, 0.02, 1200)), &c, None);
        assert_eq!(call_count(&root, "bench-team"), 1);
        let db = open_team_db(&root, "bench-team").unwrap();
        let (outcome, tin, tout, cost, wall): (String, i64, i64, f64, i64) = db.query_row(
            "SELECT outcome, tokens_in, tokens_out, cost_usd, wall_ms FROM calls LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!(outcome, "ok");
        assert_eq!(tin, 10);
        assert_eq!(tout, 5);
        assert!((cost - 0.02).abs() < 1e-9);
        assert_eq!(wall, 1200);
    }

    #[test]
    fn a_row_is_appended_per_endpoint_and_local_call() {
        let (_d, root) = ws();
        let c = ctx(&root);
        record_llm(Some(&c), &Seat::Endpoint { alias: "fake".into(), model: Some("small".into()) }, "extract", &Ok(("{\"a\":1}".into(), 0.0, 50)));
        record_llm(Some(&c), &Seat::Local { tag: "qwen".into() }, "local", &Ok(("{\"b\":2}".into(), 0.0, 80)));
        assert_eq!(call_count(&root, "bench-team"), 2);
        let db = open_team_db(&root, "bench-team").unwrap();
        let sources: Vec<String> = db.prepare("SELECT source FROM calls ORDER BY id").unwrap()
            .query_map([], |r| r.get(0)).unwrap().flatten().collect();
        assert_eq!(sources, vec!["endpoint".to_string(), "local".to_string()]);
    }

    #[test]
    fn sanitize_replaces_secrets_and_personal_data_before_a_row_is_stored() {
        let raw = "key sk-abcdefghijklmnopqrstuvwxyz123 email me@corp.com card 4111 1111 1111 1111 bearer AbCdEf123";
        let s = sanitize(raw);
        assert!(!s.contains("sk-abcdefghijklmnopqrstuvwxyz123"));
        assert!(!s.contains("me@corp.com"));
        assert!(!s.contains("4111 1111 1111 1111"));
        assert!(s.contains("[REDACTED_API_KEY]") && s.contains("[REDACTED_EMAIL]") && s.contains("[REDACTED_CARD]"));
        let (_d, root) = ws();
        record_cli(Engine::Codex, &[ChatMessage { role: "user".into(), content: raw.into() }], &Ok(ok_out(raw, 1, 1, 0.0, 1)), &ctx(&root), None);
        let db = open_team_db(&root, "bench-team").unwrap();
        let p: String = db.query_row("SELECT prompt FROM calls LIMIT 1", [], |r| r.get(0)).unwrap();
        assert!(!p.contains("sk-abcdefghijklmnopqrstuvwxyz123"));
        assert!(p.contains("[REDACTED_API_KEY]"));
    }

    #[test]
    fn with_the_off_switch_only_hashes_are_stored() {
        let (d, root) = ws();
        let team_dir = d.join(".antiloki").join("teams").join("bench-team");
        std::fs::create_dir_all(&team_dir).unwrap();
        std::fs::write(team_dir.join("ledger-off.json"), json!({"off": true}).to_string()).unwrap();
        record_cli(Engine::ClaudeCode, &[ChatMessage { role: "user".into(), content: "secret".into() }], &Ok(ok_out("answer", 1, 1, 0.0, 1)), &ctx(&root), None);
        let db = open_team_db(&root, "bench-team").unwrap();
        let (p, a, ph, ah): (Option<String>, Option<String>, String, String) = db.query_row(
            "SELECT prompt, answer, prompt_hash, answer_hash FROM calls LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert!(p.is_none() && a.is_none());
        assert!(!ph.is_empty() && !ah.is_empty());
    }

    #[test]
    fn the_off_switch_can_be_set_and_cleared_by_the_api_helper() {
        let (_d, root) = ws();
        set_off(&root, "bench-team", true).unwrap();
        assert!(ledger_off(&root, "bench-team"));
        set_off(&root, "bench-team", false).unwrap();
        assert!(!ledger_off(&root, "bench-team"));
    }

    #[test]
    fn retention_deletes_old_calls_but_keeps_golden_regression_and_trap_examples() {
        let (_d, root) = ws();
        let c = open_team_db(&root, "bench-team").unwrap();
        let old = crate::cli::now_ms() - 200 * 86_400_000;
        c.execute(
            "INSERT INTO calls (ts_ms, team, caller, source, outcome, bench, prompt_hash, answer_hash) VALUES (?1,'bench-team','x','cli','ok',1,'h','h')",
            [old]).unwrap();
        let id_keep = c.last_insert_rowid();
        c.execute(
            "INSERT INTO calls (ts_ms, team, caller, source, outcome, bench, prompt_hash, answer_hash) VALUES (?1,'bench-team','x','cli','ok',1,'h2','h2')",
            [old - 1]).unwrap();
        let id_drop = c.last_insert_rowid();
        c.execute(
            "INSERT INTO examples (cell, kind, source_call, hash) VALUES ('c','golden',?1,'g1')", [id_keep]).unwrap();
        c.execute(
            "INSERT INTO examples (cell, kind, source_call, hash) VALUES ('c','regression',?1,'g2')", [id_keep]).unwrap();
        c.execute(
            "INSERT INTO examples (cell, kind, source_call, hash) VALUES ('c','trap',?1,'g3')", [id_keep]).unwrap();
        apply_retention(&c, 90).unwrap();
        let kept: i64 = c.query_row("SELECT COUNT(*) FROM calls WHERE id = ?1", [id_keep], |r| r.get(0)).unwrap();
        let gone: i64 = c.query_row("SELECT COUNT(*) FROM calls WHERE id = ?1", [id_drop], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1);
        assert_eq!(gone, 0);
    }

    #[test]
    fn the_dataset_survives_reopening_the_file() {
        let (_d, root) = ws();
        record_cli(Engine::ClaudeCode, &[ChatMessage { role: "user".into(), content: "x".into() }], &Ok(ok_out("y", 1, 1, 0.0, 1)), &ctx(&root), None);
        drop(open_team_db(&root, "bench-team").unwrap());
        let db = open_team_db(&root, "bench-team").unwrap();
        let n: i64 = db.query_row("SELECT COUNT(*) FROM calls", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        assert!(bench_path(&root, "bench-team").exists());
    }

    #[test]
    fn a_failed_write_never_fails_the_call() {
        let (_d, root) = ws();
        test_force_fail_writes(true);
        let before = write_failures();
        record_cli(Engine::ClaudeCode, &[ChatMessage { role: "user".into(), content: "x".into() }], &Ok(ok_out("y", 1, 1, 0.0, 1)), &ctx(&root), None);
        assert!(write_failures() > before);
        test_force_fail_writes(false);
        assert_eq!(call_count(&root, "bench-team"), 0);
        record_cli(Engine::ClaudeCode, &[ChatMessage { role: "user".into(), content: "x".into() }], &Ok(ok_out("y", 1, 1, 0.0, 1)), &ctx(&root), None);
        assert_eq!(call_count(&root, "bench-team"), 1);
    }

    #[test]
    fn node_records_jsonl_is_imported_once_into_node_runs() {
        let (d, root) = ws();
        let team_dir = d.join(".antiloki").join("teams").join("bench-team");
        std::fs::create_dir_all(&team_dir).unwrap();
        let row = NodeRecord { run: "run1".into(), t_ms: 1, team: "bench-team".into(), graph: "gh".into(), node: "extract".into(), kind: "llm".into(), title: "t".into(), cfg: "c1".into(), seat: "endpoint:x".into(), tries: 1, ok: true, outcome: "ok".into(), cost_usd: 0.0, ms: 10, escalated: false, detail: json!({}) };
        let jsonl = team_dir.join("node-records.jsonl");
        std::fs::write(&jsonl, format!("{}\n", serde_json::to_string(&row).unwrap())).unwrap();
        sync_node_records(&root, "bench-team", &jsonl, &[]);
        flush();
        let db = open_team_db(&root, "bench-team").unwrap();
        let n: i64 = db.query_row("SELECT COUNT(*) FROM node_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        std::fs::write(&jsonl, format!("{}\n{}\n", serde_json::to_string(&row).unwrap(), serde_json::to_string(&NodeRecord { node: "other".into(), ..row.clone() }).unwrap())).unwrap();
        sync_node_records(&root, "bench-team", &jsonl, &[]);
        flush();
        let n2: i64 = db.query_row("SELECT COUNT(*) FROM node_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(n2, 1, "import runs once; second jsonl line alone does not duplicate the first import");
        let notes: i64 = db.query_row("SELECT COUNT(*) FROM notes WHERE kind = 'import'", [], |r| r.get(0)).unwrap();
        assert_eq!(notes, 1);
    }

    #[test]
    fn sanitize_keeps_long_numbers_that_are_not_cards_and_catches_more_key_shapes() {
        let s = sanitize("ts 1731600000000 order 1234567890123456 card 4111 1111 1111 1111 ghp_abcdefghijklmnopqrstuvwxyz0123456789 AKIAABCDEFGHIJKLMNOP");
        assert!(s.contains("1731600000000") && s.contains("1234567890123456"), "{s}");
        assert!(!s.contains("4111") && s.contains("[REDACTED_CARD]"), "{s}");
        assert!(!s.contains("ghp_") && !s.contains("AKIA"), "{s}");
        assert!(sanitize("-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----").contains("[REDACTED_PRIVATE_KEY]"));
    }

    #[test]
    fn outcomes_tell_a_timeout_a_cancel_a_refusal_and_a_plain_error_apart() {
        assert_eq!(outcome_from_cli(&Err("the call timed out".into())), "timeout");
        assert_eq!(outcome_from_cli(&Err("cancelled by the user".into())), "killed");
        assert_eq!(outcome_from_cli(&Err("blocked by policy".into())), "refused");
        assert_eq!(outcome_from_cli(&Err("spawn failed: no such file".into())), "error");
    }

    #[test]
    fn a_climb_and_the_try_number_and_the_model_are_kept_on_the_row() {
        let (_d, root) = ws();
        let mut c = ctx(&root); c.try_n = Some(2); c.climbed_from = Some("endpoint:small".into());
        record_llm(Some(&c), &Seat::Claude { model: "haiku".into() }, "p", &Ok(("{}".into(), 0.01, 5)));
        record_cli(Engine::ClaudeCode, &[ChatMessage { role: "user".into(), content: "x".into() }], &Ok(ok_out("y", 1, 1, 0.0, 1)), &ctx(&root), Some("opus".into()));
        let db = open_team_db(&root, "bench-team").unwrap();
        let (t, cf, m): (i64, String, String) = db.query_row("SELECT try, climbed_from, model FROM calls WHERE source='cli' ORDER BY id LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((t, cf.as_str(), m.as_str()), (2, "endpoint:small", "haiku"));
        let m2: String = db.query_row("SELECT model FROM calls ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(m2, "opus");
    }

    #[test]
    fn a_call_outside_any_team_lands_in_the_workspace_dataset() {
        let (_d, root) = ws();
        let c = Ctx::workspace(&root, "frames");
        record_cli(Engine::ClaudeCode, &[ChatMessage { role: "user".into(), content: "x".into() }], &Ok(ok_out("y", 1, 1, 0.0, 1)), &c, None);
        assert_eq!(call_count(&root, WORKSPACE_TEAM), 1);
        assert!(bench_path(&root, WORKSPACE_TEAM).exists());
    }
}