//! timeline-stats.rs — twin of skills/audit/timeline-stats.mjs (db READ lane). NAMES ARE
//! CONTRACT: `timelineStats` (default, standard lane). Deterministic GROUP BY aggregates over the
//! sealed sacred_timeline chain: totals, per-day, by-operation/entity_type/actor/exec_env,
//! by-mission (joined to the interviewer turn that caused the work), by-card, plus an archived-
//! segment census.
//!
//! `chainLayout`/`chainTableRef` are inlined here (not converted from
//! skills/audit/_helpers/timeline-segments.mjs) following the precedent set by verify-chain.rs —
//! that helper module also does real filesystem I/O out of scope for a shared twin.
use rusqlite::Connection;
use serde_json::{json, Value};

fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}

// chainLayout: where the chain actually lives on this connection (timeline-segments.mjs).
fn chain_layout(db: &Connection) -> &'static str {
    let has = |schema: &str| -> bool {
        let sql = format!("SELECT 1 FROM {schema}.sqlite_master WHERE type='table' AND name='sacred_timeline'");
        db.query_row(&sql, [], |_| Ok(())).is_ok()
    };
    let in_timeline = has("timeline");
    let in_main = has("main");
    if in_timeline && in_main { "both" } else if in_timeline { "timeline" } else if in_main { "legacy-main" } else { "none" }
}

// chainTableRef: qualified table name for the chain's active home, or None on ambiguity/absence.
fn chain_table_ref(db: &Connection) -> Option<String> {
    match chain_layout(db) {
        "timeline" => Some("timeline.sacred_timeline".to_string()),
        "legacy-main" => Some("main.sacred_timeline".to_string()),
        "both" => match db.query_row::<i64, _, _>("SELECT COUNT(*) n FROM timeline.sacred_timeline", [], |r| r.get(0)) {
            Ok(0) => Some("main.sacred_timeline".to_string()),
            _ => None,
        },
        _ => None,
    }
}

// timelinePathFromDb: the on-disk path of the attached 'timeline' DB, or None for an in-memory
// timeline. NOTE: the conform-db harness's cross-db lane attaches `ATTACH ':memory:' AS timeline`
// for BOTH JS and Rust lanes, so `file` is always empty here and this always resolves to None —
// the archived-segment branch below is therefore unreachable via fixture (real fs I/O against a
// live workspace directory is not fixture-portable; the JS wraps it in try/catch and degrades to
// an empty census on any failure, which this twin mirrors faithfully but cannot exercise in tests).
fn timeline_path_from_db(db: &Connection) -> Option<String> {
    let file: Option<String> = db
        .query_row("SELECT file FROM pragma_database_list WHERE name = 'timeline'", [], |r| r.get::<_, Option<String>>(0))
        .ok()
        .flatten();
    file.filter(|f| !f.is_empty())
}

// discoverSegments(dirname(tlPath)): archived timeline-NNNN-*.sqlite[.gz] files, oldest → newest.
fn discover_segments(db_dir: &std::path::Path) -> Vec<Value> {
    let legacy_dir = db_dir.join("legacy_timeline");
    let entries = match std::fs::read_dir(&legacy_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let re = regex::Regex::new(r"^timeline-(\d+)-.*\.sqlite(\.gz)?$").unwrap();
    let mut segs: Vec<(u64, String, bool)> = Vec::new();
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if let Some(caps) = re.captures(name) {
                let seq: u64 = caps[1].parse().unwrap_or(0);
                segs.push((seq, name.to_string(), caps.get(2).is_some()));
            }
        }
    }
    segs.sort_by_key(|s| s.0);
    segs.into_iter().map(|(seq, file, gz)| json!({ "seq": seq, "file": file, "compressed": gz })).collect()
}

pub fn timelineStats(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { days = null } = input ?? {}` — default fires only on a missing/undefined key;
    // a present `null` stays null either way, so a plain get-or-null read is equivalent.
    let days_val = input.get("days").cloned().unwrap_or(Value::Null);

    let ref_ = chain_table_ref(db).ok_or_else(|| "timeline-stats: no sacred_timeline table on this connection".to_string())?;

    let since: Option<String> = if !days_val.is_null() && js_number(Some(&days_val)) > 0.0 {
        let floor_days = js_number(Some(&days_val)).floor() as i64;
        let modifier = format!("-{floor_days} days");
        let rows = crate::db_json::query_rows(db, &format!("SELECT datetime({}, ?) AS t", crate::clock::now_sql_inner()), &[json!(modifier)])?;
        rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("t")).and_then(|v| v.as_str()).map(|s| s.to_string())
    } else {
        None
    };

    let w = if since.is_some() { "WHERE ts >= ?" } else { "" };
    let args: Vec<Value> = since.clone().map(|s| vec![json!(s)]).unwrap_or_default();

    let totals_sql = format!(
        "SELECT COUNT(*) rows, MIN(ts) first_ts, MAX(ts) last_ts, \
         COUNT(DISTINCT actor) actors, \
         COUNT(DISTINCT entity_type || ':' || entity_id) entities, \
         COUNT(DISTINCT CASE WHEN user_goal_id IS NOT NULL THEN user_goal_id END) missions, \
         SUM(CASE WHEN user_goal_id IS NULL THEN 1 ELSE 0 END) background_rows \
         FROM {ref_} {w}"
    );
    let totals = crate::db_json::query_rows(db, &totals_sql, &args)?
        .as_array().and_then(|a| a.first().cloned()).unwrap_or(json!({}));

    let per_day_sql = format!("SELECT substr(ts, 1, 10) d, COUNT(*) n FROM {ref_} {w} GROUP BY d ORDER BY d ASC");
    let per_day = crate::db_json::query_rows(db, &per_day_sql, &args)?;

    let agg = |col: &str, n: usize| -> Result<Value, String> {
        let sql = format!("SELECT {col} k, COUNT(*) n FROM {ref_} {w} GROUP BY {col} ORDER BY n DESC, k ASC");
        let rows = crate::db_json::query_rows(db, &sql, &args)?;
        let arr = rows.as_array().cloned().unwrap_or_default();
        Ok(json!(arr.into_iter().take(n).collect::<Vec<_>>()))
    };

    let by_operation = agg("operation", 20)?;
    let by_entity_type = agg("entity_type", 12)?;
    let by_actor = agg("actor", 12)?;

    let by_exec_env_sql = format!("SELECT COALESCE(exec_env, '(none)') k, COUNT(*) n FROM {ref_} {w} GROUP BY k ORDER BY n DESC");
    let by_exec_env_rows = crate::db_json::query_rows(db, &by_exec_env_sql, &args)?;
    let by_exec_env = json!(by_exec_env_rows.as_array().cloned().unwrap_or_default().into_iter().take(8).collect::<Vec<_>>());

    let mission_where = if since.is_some() { "WHERE t.ts >= ? AND" } else { "WHERE" };
    let by_mission_sql = format!(
        "SELECT t.user_goal_id k, COUNT(*) n, \
         MIN(t.ts) first_ts, MAX(t.ts) last_ts, \
         (SELECT it.user_text FROM main.interviewer_turns it WHERE it.id = t.user_goal_id) goal_text \
         FROM {ref_} t \
         {mission_where} t.user_goal_id IS NOT NULL \
         GROUP BY t.user_goal_id ORDER BY n DESC LIMIT 15"
    );
    let by_mission = crate::db_json::query_rows(db, &by_mission_sql, &args)?;

    let card_where = if since.is_some() { "WHERE ts >= ? AND" } else { "WHERE" };
    let by_card_sql = format!(
        "SELECT entity_id k, COUNT(*) n, MIN(ts) first_ts, MAX(ts) last_ts \
         FROM {ref_} \
         {card_where} entity_type = 'card' \
         GROUP BY entity_id ORDER BY n DESC LIMIT 15"
    );
    let by_card = crate::db_json::query_rows(db, &by_card_sql, &args)?;

    // Archive census — see timeline_path_from_db's note: always empty under the fixture harness.
    let mut segments: Vec<Value> = Vec::new();
    if let Some(tl_path) = timeline_path_from_db(db) {
        if let Some(dir) = std::path::Path::new(&tl_path).parent() {
            segments = discover_segments(dir);
        }
    }

    Ok(json!({
        "ok": true,
        "layout": chain_layout(db),
        "window_days": days_val,
        "since": since,
        "totals": totals,
        "per_day": per_day,
        "by_operation": by_operation,
        "by_entity_type": by_entity_type,
        "by_actor": by_actor,
        "by_exec_env": by_exec_env,
        "by_mission": by_mission,
        "by_card": by_card,
        "archived_segments": segments,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS timeline;").unwrap();
        c.execute_batch(
            "CREATE TABLE timeline.sacred_timeline (
               id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, actor TEXT NOT NULL,
               operation TEXT NOT NULL, entity_type TEXT NOT NULL, entity_id TEXT NOT NULL,
               before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT NOT NULL,
               digital_print TEXT NOT NULL UNIQUE, user_goal_id INTEGER, exec_env TEXT, tick_id INTEGER
             );
             CREATE TABLE main.interviewer_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT);
             CREATE TABLE main.interviewer_turns (
               id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL,
               turn_index INTEGER NOT NULL, user_text TEXT NOT NULL, classified_kind TEXT NOT NULL
             );",
        )
        .unwrap();
        c
    }

    fn seal(c: &Connection, entity_id: &str, goal_id: Option<i64>, n: i64) {
        c.execute(
            "INSERT INTO timeline.sacred_timeline (ts, actor, operation, entity_type, entity_id, after_json, prev_print, digital_print, user_goal_id) \
             VALUES ('2026-01-01T00:00:00.000Z', 'tester', 'card.add', 'card', ?1, '{}', 'GENESIS', ?2, ?3)",
            rusqlite::params![entity_id, format!("print-{n}"), goal_id],
        )
        .unwrap();
    }

    #[test]
    fn totals_breakdowns_missions_cards() {
        let c = seeded_db();
        c.execute("INSERT INTO interviewer_sessions (id) VALUES (1)", []).unwrap();
        c.execute(
            "INSERT INTO interviewer_turns (id, session_id, turn_index, user_text, classified_kind) VALUES (77, 1, 0, 'ship the login screen', 'propose')",
            [],
        )
        .unwrap();
        for i in 0..4 { seal(&c, &format!("CARD-A{i}"), Some(77), i); }
        for i in 0..2 { seal(&c, &format!("CARD-B{i}"), None, 100 + i); }

        let r = timelineStats(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["totals"]["rows"], json!(6));
        assert_eq!(r["totals"]["missions"], json!(1));
        assert_eq!(r["totals"]["background_rows"], json!(2));
        assert_eq!(r["per_day"].as_array().unwrap().len(), 1);
        assert_eq!(r["per_day"][0]["n"], json!(6));
        assert_eq!(r["by_operation"][0]["k"], json!("card.add"));
        assert_eq!(r["by_operation"][0]["n"], json!(6));
        assert_eq!(r["by_entity_type"][0]["k"], json!("card"));
        assert_eq!(r["by_mission"].as_array().unwrap().len(), 1);
        assert_eq!(r["by_mission"][0]["k"], json!(77));
        assert_eq!(r["by_mission"][0]["n"], json!(4));
        assert_eq!(r["by_mission"][0]["goal_text"], json!("ship the login screen"));
        assert_eq!(r["by_card"].as_array().unwrap().len(), 6);
        assert!(r["by_card"].as_array().unwrap().iter().all(|c| c["n"] == json!(1)));
        assert!(r["by_card"].as_array().unwrap().iter().any(|c| c["k"] == json!("CARD-A0")));
        assert_eq!(r["layout"], json!("timeline"));
        assert_eq!(r["archived_segments"], json!([]));
    }

    #[test]
    fn positive_window_sets_a_cutoff_without_excluding_todays_rows() {
        let c = seeded_db();
        seal(&c, "CARD-C0", None, 1);
        let r = timelineStats(&json!({ "days": 7 }), &c).unwrap();
        assert_eq!(r["totals"]["rows"], json!(1));
        assert!(r["since"].as_str().is_some());
        assert_eq!(r["window_days"], json!(7));
    }

    #[test]
    fn zero_or_negative_days_is_ignored() {
        let c = seeded_db();
        seal(&c, "CARD-D0", None, 1);
        let r = timelineStats(&json!({ "days": 0 }), &c).unwrap();
        assert_eq!(r["since"], Value::Null);
        let r2 = timelineStats(&json!({ "days": -3 }), &c).unwrap();
        assert_eq!(r2["since"], Value::Null);
    }

    #[test]
    fn missing_sacred_timeline_table_errors() {
        let c = Connection::open_in_memory().unwrap();
        let e = timelineStats(&json!({}), &c).unwrap_err();
        assert_eq!(e, "timeline-stats: no sacred_timeline table on this connection");
    }
}
