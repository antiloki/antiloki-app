//! EPISODES LIVE IN THE REPO'S MEMORY (operator 2026-09-24: *"all the memory and episodes should live and be
//! consolidated in a sqlite db on the repo. Remember that all antiloki's personal memory is on the sqlite dbs at
//! the user's repo? by doing that we can solid consolidate the episodes."*).
//!
//! They did not. An episode was reconstructed every time by re-parsing `agent-events-<pane>.jsonl` — so it existed
//! only as long as that file did, and once agents got boxes of their own the file moved INTO the box, where
//! removing the worktree would take every episode with it. Meanwhile `<repo>/.antiloki/db/memory.sqlite` already
//! held antiloki's memory for that workspace, with a `memory_consolidation` table whose columns
//! (`period_start`, `calls_count`, `overview_md`, `journey_md`, …) describe an episode almost exactly — because
//! an episode IS a consolidation, which is what the operator has been saying since it was named.
//!
//! So a closed episode is a `memory_consolidation` row with `source = 'episode'`, written to the WORKSPACE's
//! database no matter which box the agent worked in. One place, one schema, surviving the box.
//!
//! The event log stays exactly as it is. It is the append-only deterministic record and the only thing that can
//! reconstruct a turn; this is the consolidation of it, not a replacement for it.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// `<workspace>/.antiloki/db/memory.sqlite` — the repo's own memory, never a box's.
pub fn db_path(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("db").join("memory.sqlite") }

/// The consolidation table, and the columns an episode needs on top of what a call-digest needs.
///
/// `memory_consolidation` is shared with `brain-consolidate` (`source = 'ai_calls'`), so nothing here drops or
/// rewrites anything: the table is created if this workspace has never had one, and the episode columns are added
/// if absent. `ALTER TABLE ADD COLUMN` on sqlite is O(1) and leaves existing rows NULL, which is the truth.
pub fn ensure(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_consolidation (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           created_at TEXT NOT NULL DEFAULT (datetime('now')),
           source TEXT NOT NULL DEFAULT 'ai_calls',
           period_start TEXT NOT NULL, period_end TEXT NOT NULL,
           calls_count INTEGER NOT NULL,
           cost_usd_total REAL, tokens_in_total INTEGER, tokens_out_total INTEGER,
           overview_md TEXT NOT NULL, analysis_md TEXT, keypoints_json TEXT,
           learnings_md TEXT, fails_md TEXT, journey_md TEXT, meta_json TEXT, pruned_at TEXT)")
        .map_err(|e| format!("memory_consolidation: {e}"))?;
    // the episode's own identity. Errors are ignored one by one: a column that is already there is the normal case
    for col in ["agent_slug TEXT", "pane_id INTEGER", "episode_n INTEGER", "engine TEXT",
                "title TEXT", "subtitle TEXT", "branch TEXT", "merged_into TEXT", "turns INTEGER"] {
        let _ = db.execute(&format!("ALTER TABLE memory_consolidation ADD COLUMN {col}"), []);
    }
    // one episode per agent per number: closing twice must update, never duplicate
    let _ = db.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS memory_consolidation_episode
        ON memory_consolidation (agent_slug, episode_n) WHERE source = 'episode'");
    Ok(())
}

/// `1790259951906` → `2026-09-24 14:25:51` (UTC, the format the rest of this table uses). Pure — tested.
pub fn stamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// One closed episode, as it goes into the repo's memory.
#[derive(Debug, Clone, Default)]
pub struct Closed {
    pub agent_slug: String, pub pane_id: i64, pub n: usize, pub engine: String,
    pub title: String, pub subtitle: String, pub snapshot: String, pub story: String,
    pub first_t: i64, pub last_t: i64, pub turns: usize,
    pub branch: String, pub merged_into: String,
}

/// Write it. Closing the same episode twice updates the row rather than making a second one — a re-close after a
/// merge that failed is a correction, not a new chapter.
pub fn record(root: &str, e: &Closed) -> Result<i64, String> {
    let p = db_path(root);
    std::fs::create_dir_all(p.parent().ok_or("no db directory")?).map_err(|x| x.to_string())?;
    let db = Connection::open(&p).map_err(|x| format!("{}: {x}", p.display()))?;
    let _ = db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
    ensure(&db)?;
    db.execute(
        "INSERT INTO memory_consolidation
           (source, period_start, period_end, calls_count, overview_md, journey_md,
            agent_slug, pane_id, episode_n, engine, title, subtitle, branch, merged_into, turns)
         VALUES ('episode', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?3)
         ON CONFLICT (agent_slug, episode_n) WHERE source = 'episode' DO UPDATE SET
            period_end = excluded.period_end, calls_count = excluded.calls_count,
            overview_md = excluded.overview_md, journey_md = excluded.journey_md,
            title = excluded.title, subtitle = excluded.subtitle,
            branch = excluded.branch, merged_into = excluded.merged_into, turns = excluded.turns",
        rusqlite::params![stamp(e.first_t), stamp(e.last_t), e.turns as i64, e.snapshot, e.story,
                          e.agent_slug, e.pane_id, e.n as i64, e.engine, e.title, e.subtitle, e.branch, e.merged_into])
        .map_err(|x| format!("recording episode {} of {}: {x}", e.n, e.agent_slug))?;
    Ok(db.last_insert_rowid())
}

/// Every consolidated episode in this workspace, newest last-turn first. `agent` narrows it to one.
pub fn list(root: &str, agent: Option<&str>, limit: usize) -> Vec<Value> {
    let p = db_path(root);
    if !p.exists() { return vec![] }
    let Ok(db) = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
    let sql = format!(
        "SELECT id, agent_slug, pane_id, episode_n, engine, title, subtitle, overview_md, journey_md,
                period_start, period_end, turns, branch, merged_into
           FROM memory_consolidation
          WHERE source = 'episode' {}
          ORDER BY period_end DESC LIMIT {limit}",
        if agent.is_some() { "AND agent_slug = ?1" } else { "" });
    let Ok(mut st) = db.prepare(&sql) else { return vec![] };
    let row = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
        Ok(json!({ "id": r.get::<_, i64>(0)?, "agent": r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            "pane_id": r.get::<_, Option<i64>>(2)?.unwrap_or(0), "n": r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            "engine": r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            "title": r.get::<_, Option<String>>(5)?.unwrap_or_default(), "subtitle": r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            "snapshot": r.get::<_, Option<String>>(7)?.unwrap_or_default(), "story": r.get::<_, Option<String>>(8)?.unwrap_or_default(),
            "first": r.get::<_, Option<String>>(9)?.unwrap_or_default(), "last": r.get::<_, Option<String>>(10)?.unwrap_or_default(),
            "turns": r.get::<_, Option<i64>>(11)?.unwrap_or(0),
            "branch": r.get::<_, Option<String>>(12)?.unwrap_or_default(), "merged_into": r.get::<_, Option<String>>(13)?.unwrap_or_default() }))
    };
    let got = match agent {
        Some(a) => st.query_map(rusqlite::params![a], row).map(|i| i.flatten().collect::<Vec<_>>()),
        None => st.query_map([], row).map(|i| i.flatten().collect::<Vec<_>>()),
    };
    got.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    static NTH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn workspace() -> PathBuf {
        let n = NTH.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("antiloki-epdb-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn ep(slug: &str, n: usize, t: i64) -> Closed {
        Closed { agent_slug: slug.into(), pane_id: 24, n, engine: "claude-code".into(),
            title: format!("episode {n}"), subtitle: "what it was about".into(),
            snapshot: "the deterministic transcript".into(), story: "the engine's narrative".into(),
            first_t: t, last_t: t + 60_000, turns: 6, branch: format!("antiloki/{slug}/ep-{n}"), merged_into: "main".into() }
    }

    #[test]
    fn the_stamp_matches_the_format_the_table_already_uses() {
        assert_eq!(stamp(0), "1970-01-01 00:00:00");
        assert_eq!(stamp(1_790_259_951_906), "2026-09-24 14:25:51");
        // the one memory this schema already held was written "2026-09-17 16:08:25" — same shape
        assert_eq!(stamp(1_789_675_705_000).len(), 19);
    }

    #[test]
    fn an_episode_is_a_consolidation_in_the_repos_own_memory() {
        let d = workspace(); let root = d.to_string_lossy().to_string();
        let id = record(&root, &ep("daniboy", 1, 1_790_000_000_000)).unwrap();
        assert!(id > 0);
        assert!(db_path(&root).exists(), "written to <repo>/.antiloki/db/memory.sqlite, not to a box");

        let got = list(&root, None, 50);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["agent"], json!("daniboy"));
        assert_eq!(got[0]["n"], json!(1));
        assert_eq!(got[0]["turns"], json!(6));
        assert_eq!(got[0]["snapshot"], json!("the deterministic transcript"));
        assert_eq!(got[0]["story"], json!("the engine's narrative"));
        assert_eq!(got[0]["branch"], json!("antiloki/daniboy/ep-1"));
        assert_eq!(got[0]["merged_into"], json!("main"));
        assert_eq!(got[0]["first"], json!("2026-09-21 14:13:20"), "UTC, the way the table already writes them");

        // …and it is a row of the SAME table brain-consolidate writes, which is the point
        let db = Connection::open(db_path(&root)).unwrap();
        let src: String = db.query_row("SELECT source FROM memory_consolidation WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(src, "episode");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn closing_the_same_episode_twice_corrects_it_rather_than_forking_it() {
        let d = workspace(); let root = d.to_string_lossy().to_string();
        record(&root, &ep("dani", 1, 1_790_000_000_000)).unwrap();
        // the first close merged nothing (a dirty workspace); the second one landed it
        let mut again = ep("dani", 1, 1_790_000_000_000);
        again.merged_into = "main".into(); again.title = "the corrected title".into(); again.turns = 9;
        record(&root, &again).unwrap();

        let got = list(&root, None, 50);
        assert_eq!(got.len(), 1, "one episode, not two");
        assert_eq!(got[0]["title"], json!("the corrected title"));
        assert_eq!(got[0]["turns"], json!(9));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn every_agents_episodes_land_in_one_place_and_can_be_read_back_per_agent() {
        let d = workspace(); let root = d.to_string_lossy().to_string();
        record(&root, &ep("alice", 1, 1_790_000_000_000)).unwrap();
        record(&root, &ep("alice", 2, 1_790_000_200_000)).unwrap();
        record(&root, &ep("bob", 1, 1_790_000_100_000)).unwrap();

        let all = list(&root, None, 50);
        assert_eq!(all.len(), 3, "three agents' work, one database");
        assert_eq!(all[0]["agent"], json!("alice"));
        assert_eq!(all[0]["n"], json!(2), "newest last-turn first");

        let mine = list(&root, Some("bob"), 50);
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0]["agent"], json!("bob"));
        assert_eq!(list(&root, Some("nobody"), 50).len(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_existing_memory_database_is_extended_never_rewritten() {
        let d = workspace(); let root = d.to_string_lossy().to_string();
        std::fs::create_dir_all(d.join(".antiloki/db")).unwrap();
        // a workspace that already has memory: the real schema, with a call-digest in it
        let db = Connection::open(db_path(&root)).unwrap();
        db.execute_batch("CREATE TABLE memory_consolidation (id INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL DEFAULT (datetime('now')), source TEXT NOT NULL DEFAULT 'ai_calls', period_start TEXT NOT NULL, period_end TEXT NOT NULL, calls_count INTEGER NOT NULL, cost_usd_total REAL, tokens_in_total INTEGER, tokens_out_total INTEGER, overview_md TEXT NOT NULL, analysis_md TEXT, keypoints_json TEXT, learnings_md TEXT, fails_md TEXT, journey_md TEXT, meta_json TEXT, pruned_at TEXT);
             INSERT INTO memory_consolidation (source, period_start, period_end, calls_count, overview_md) VALUES ('ai_calls','2026-09-01','2026-09-02', 40, 'the calls digest');").unwrap();
        drop(db);

        record(&root, &ep("dani", 1, 1_790_000_000_000)).unwrap();

        let db = Connection::open(db_path(&root)).unwrap();
        let calls: i64 = db.query_row("SELECT COUNT(*) FROM memory_consolidation WHERE source = 'ai_calls'", [], |r| r.get(0)).unwrap();
        assert_eq!(calls, 1, "the existing digest is untouched");
        assert_eq!(list(&root, None, 50).len(), 1, "and the episode sits beside it");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_workspace_with_no_memory_yet_reads_as_empty_not_as_an_error() {
        assert_eq!(list("/definitely/not/a/workspace", None, 10).len(), 0);
    }
}

// ── THE REPO'S OWN RECORD (operator 2026-09-24: "those sqlites gives us abilities to query and generate stats
//    deterministically with all prompts and all responses, this is what timeline can retrieve to show real
//    history sacred as well") ─────────────────────────────────────────────────────────────────────────────────
//
// `brain.sqlite` holds every AI call this workspace ever made — 611 of them on the operator's own box — with its
// template, tier, provider, model, duration, tokens, cost and outcome. That is the deterministic history: not a
// reconstruction from logs, not a model's account of itself, a row written when the call happened. The timeline
// reads it rather than walking jsonl files, which is why a day's numbers cannot drift from what actually ran.

/// `<workspace>/.antiloki/db/brain.sqlite`.
pub fn brain_path(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("db").join("brain.sqlite") }

/// Every AI call, rolled up per local day. `tz_offset_min` is minutes EAST of UTC, so a day breaks at the
/// operator's own midnight — the same rule `days.rs` uses for episodes.
pub fn call_days(root: &str, tz_offset_min: i64, limit_days: usize) -> Vec<Value> {
    let p = brain_path(root);
    if !p.exists() { return vec![] }
    let Ok(db) = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
    // the shift is applied in SQL so sqlite does the grouping; `ts` is 'YYYY-MM-DD HH:MM:SS.mmm' UTC
    let sql = format!(
        "SELECT date(ts, '{:+} minutes') AS d,
                COUNT(*), COALESCE(SUM(cost_usd_actual),0), COALESCE(SUM(tokens_in),0), COALESCE(SUM(tokens_out),0),
                COALESCE(SUM(duration_ms),0), SUM(CASE WHEN outcome != 'success' THEN 1 ELSE 0 END),
                COUNT(DISTINCT model_id)
           FROM ai_calls WHERE ts IS NOT NULL
          GROUP BY d ORDER BY d DESC LIMIT {limit_days}", tz_offset_min);
    let Ok(mut st) = db.prepare(&sql) else { return vec![] };
    st.query_map([], |r| Ok(json!({
        "day": r.get::<_, String>(0)?, "calls": r.get::<_, i64>(1)?, "cost": r.get::<_, f64>(2)?,
        "tokens_in": r.get::<_, i64>(3)?, "tokens_out": r.get::<_, i64>(4)?,
        "ms": r.get::<_, f64>(5)?, "failed": r.get::<_, i64>(6)?, "models": r.get::<_, i64>(7)?,
    }))).map(|i| i.flatten().collect()).unwrap_or_default()
}

/// Every call in a window, per model — the rows fuel is built from. `since_days` counts back from now in the
/// viewer's own days; 0 means everything ever.
pub fn calls_by_model(root: &str, since_days: i64, tz_offset_min: i64) -> Vec<Value> {
    let p = brain_path(root);
    if !p.exists() { return vec![] }
    let Ok(db) = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
    let window = if since_days > 0 {
        format!("WHERE datetime(ts, '{:+} minutes') >= datetime('now', '{:+} minutes', '-{since_days} days')", tz_offset_min, tz_offset_min)
    } else { String::new() };
    let sql = format!(
        "SELECT COALESCE(model_id,'?'), COALESCE(tier,'?'), COUNT(*), COALESCE(SUM(cost_usd_actual),0),
                COALESCE(SUM(tokens_in),0), COALESCE(SUM(tokens_out),0), COALESCE(SUM(duration_ms),0),
                SUM(CASE WHEN outcome != 'success' THEN 1 ELSE 0 END)
           FROM ai_calls {window} GROUP BY model_id, tier ORDER BY SUM(cost_usd_actual) DESC LIMIT 60");
    let Ok(mut st) = db.prepare(&sql) else { return vec![] };
    st.query_map([], |r| { let model: String = r.get(0)?; Ok(json!({
        "model": model.clone(), "house": crate::fuel::house_of(&model), "tier": r.get::<_, String>(1)?,
        "calls": r.get::<_, i64>(2)?, "cost": r.get::<_, f64>(3)?,
        "tokens_in": r.get::<_, i64>(4)?, "tokens_out": r.get::<_, i64>(5)?,
        "tokens": r.get::<_, i64>(4)? + r.get::<_, i64>(5)?,
        "ms": r.get::<_, f64>(6)?, "failed": r.get::<_, i64>(7)?,
    })) }).map(|i| i.flatten().collect()).unwrap_or_default()
}

/// Tokens per local day and model over the last `since_days` days: (day `YYYY-MM-DD`, model, tokens in, tokens out). The per-day line of the usage window prices these at OpenRouter's list.
pub fn calls_by_day_model(root: &str, since_days: i64, tz_offset_min: i64) -> Vec<(String, String, i64, i64)> {
    let p = brain_path(root); if !p.exists() || since_days <= 0 { return vec![] }
    let Ok(db) = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
    let sql = format!("SELECT date(ts, '{:+} minutes'), COALESCE(model_id,'?'), COALESCE(SUM(tokens_in),0), COALESCE(SUM(tokens_out),0) FROM ai_calls WHERE datetime(ts, '{:+} minutes') >= datetime('now', '{:+} minutes', '-{since_days} days') GROUP BY 1, 2 ORDER BY 1", tz_offset_min, tz_offset_min, tz_offset_min);
    let Ok(mut st) = db.prepare(&sql) else { return vec![] };
    st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))).map(|i| i.flatten().collect()).unwrap_or_default()
}

/// The same, for the window BEFORE this one — what "yesterday" means in the comparison.
pub fn calls_by_model_prior(root: &str, since_days: i64, tz_offset_min: i64) -> Vec<Value> {
    let p = brain_path(root);
    if !p.exists() || since_days <= 0 { return vec![] }
    let Ok(db) = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
    let sql = format!(
        "SELECT COALESCE(model_id,'?'), COUNT(*), COALESCE(SUM(cost_usd_actual),0), COALESCE(SUM(tokens_in+tokens_out),0)
           FROM ai_calls
          WHERE datetime(ts, '{tz:+} minutes') <  datetime('now', '{tz:+} minutes', '-{since_days} days')
            AND datetime(ts, '{tz:+} minutes') >= datetime('now', '{tz:+} minutes', '-{} days')
          GROUP BY model_id", since_days * 2, tz = tz_offset_min);
    let Ok(mut st) = db.prepare(&sql) else { return vec![] };
    st.query_map([], |r| { let model: String = r.get(0)?; Ok(json!({
        "model": model.clone(), "house": crate::fuel::house_of(&model),
        "calls": r.get::<_, i64>(1)?, "cost": r.get::<_, f64>(2)?, "tokens": r.get::<_, i64>(3)?,
    })) }).map(|i| i.flatten().collect()).unwrap_or_default()
}

/// What ran on ONE day, model by model — the answer to "where did that day's money go", straight from the rows.
pub fn call_breakdown(root: &str, day: &str, tz_offset_min: i64) -> Vec<Value> {
    let p = brain_path(root);
    if !p.exists() { return vec![] }
    let Ok(db) = Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
    let sql = format!(
        "SELECT COALESCE(model_id,'?'), COALESCE(tier,'?'), COUNT(*), COALESCE(SUM(cost_usd_actual),0),
                COALESCE(SUM(tokens_in+tokens_out),0)
           FROM ai_calls WHERE date(ts, '{:+} minutes') = ?1
          GROUP BY model_id, tier ORDER BY SUM(cost_usd_actual) DESC LIMIT 40", tz_offset_min);
    let Ok(mut st) = db.prepare(&sql) else { return vec![] };
    st.query_map(rusqlite::params![day], |r| Ok(json!({
        "model": r.get::<_, String>(0)?, "tier": r.get::<_, String>(1)?, "calls": r.get::<_, i64>(2)?,
        "cost": r.get::<_, f64>(3)?, "tokens": r.get::<_, i64>(4)?,
    }))).map(|i| i.flatten().collect()).unwrap_or_default()
}

#[cfg(test)]
mod call_tests {
    use super::*;

    fn brain_with(root: &str, rows: &[(&str, &str, &str, f64, i64, i64, &str)]) {
        std::fs::create_dir_all(Path::new(root).join(".antiloki/db")).unwrap();
        let db = Connection::open(brain_path(root)).unwrap();
        db.execute_batch("CREATE TABLE ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, prompt_template_id INTEGER, tier TEXT, provider TEXT, model_id TEXT, duration_ms REAL, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, outcome TEXT)").unwrap();
        for (ts, tier, model, cost, tin, tout, outcome) in rows {
            db.execute("INSERT INTO ai_calls (ts, tier, model_id, duration_ms, cost_usd_actual, tokens_in, tokens_out, outcome) VALUES (?1,?2,?3,1000,?4,?5,?6,?7)",
                rusqlite::params![ts, tier, model, cost, tin, tout, outcome]).unwrap();
        }
    }

    #[test]
    fn the_days_numbers_come_from_the_rows_that_were_written_when_the_calls_happened() {
        let d = std::env::temp_dir().join(format!("antiloki-brain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let root = d.to_string_lossy().to_string();
        brain_with(&root, &[
            ("2026-09-24 16:00:54.268", "low",  "claude-code:haiku",  0.024, 9770, 1658, "success"),
            ("2026-09-24 17:10:00.000", "mid",  "claude-code:sonnet", 0.310, 1000,  200, "success"),
            ("2026-09-24 02:00:00.000", "low",  "opencode:flash",     0.001,  100,   20, "error"),
            ("2026-09-23 12:00:00.000", "high", "claude-code:opus",   1.280, 5000, 1000, "success"),
        ]);

        let utc = call_days(&root, 0, 30);
        assert_eq!(utc.len(), 2);
        assert_eq!(utc[0]["day"], json!("2026-09-24"));
        assert_eq!(utc[0]["calls"], json!(3));
        assert_eq!(utc[0]["failed"], json!(1), "a call that did not succeed is counted as itself");
        assert_eq!(utc[0]["models"], json!(3));
        assert!((utc[0]["cost"].as_f64().unwrap() - 0.335).abs() < 1e-9);
        assert_eq!(utc[0]["tokens_in"], json!(10_870));

        // A DAY BREAKS AT THE OPERATOR'S MIDNIGHT. At UTC-3 the 02:00 UTC call belongs to the 23rd, not the 24th.
        let brasilia = call_days(&root, -180, 30);
        assert_eq!(brasilia[0]["day"], json!("2026-09-24"));
        assert_eq!(brasilia[0]["calls"], json!(2), "the 02:00 UTC call moved to the day before");
        assert_eq!(brasilia[1]["calls"], json!(2));

        // and the breakdown answers "where did the money go", biggest first
        let b = call_breakdown(&root, "2026-09-24", 0);
        assert_eq!(b[0]["model"], json!("claude-code:sonnet"));
        assert_eq!(b[0]["tier"], json!("mid"));
        assert_eq!(b[1]["model"], json!("claude-code:haiku"));
        assert_eq!(b[0]["tokens"], json!(1200));

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_workspace_that_has_never_made_a_call_reads_as_empty() {
        assert_eq!(call_days("/definitely/nowhere", 0, 30).len(), 0);
        assert_eq!(call_breakdown("/definitely/nowhere", "2026-09-24", 0).len(), 0);
    }
}
