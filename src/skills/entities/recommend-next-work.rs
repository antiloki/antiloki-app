//! recommend-next-work.rs — twin of skills/entities/recommend-next-work.mjs (db READ lane).
//! NAMES ARE CONTRACT: `recommendNextWork` (named default). Rule-based backlog scorer, pure read.
//! Frozen-clock lane: JS `Date.now()`/`new Date()` are pinned to 2026-01-01T00:00:00.000Z, so the
//! twin writes those literals by hand (1767225600000 ms / the ISO string) to stay byte-identical.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

// Date.now() — real wall-clock in production, the pinned instant under test/conform.
fn frozen_now_ms() -> i64 { crate::clock::now_ms() }
const FOURTEEN_DAYS_MS: i64 = 14 * 86400000;

// JS truthiness of `v` (used for `!x` and `x && ...`).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `??` right operand: nz(a).or(nz(b)) — null/undefined fall through.
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// JS Number(v).
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
// JS Math.round: nearest integer, ties toward +Infinity (spec-correct, avoids the x+0.5 float bug).
fn js_round(x: f64) -> f64 {
    if !x.is_finite() { return x; }
    let f = x.floor();
    if x - f < 0.5 { f } else { f + 1.0 }
}

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// Parse an ISO-8601 UTC instant ('YYYY-MM-DD' or 'YYYY-MM-DDTHH:MM:SS[.sss][Z]') to epoch ms.
// Every call-site feeds a Z-suffixed or date-only string → interpreted as UTC, matching
// `new Date(...).getTime()`. Any trailing 'Z' is ignored (already UTC); a malformed string → None
// (mirrors JS `NaN`, where `now - NaN > x` and `NaN > x` are both false).
fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' { return None; }
    let num = |lo: usize, hi: usize| -> Option<i64> {
        let sub = s.get(lo..hi)?;
        if sub.bytes().all(|c| c.is_ascii_digit()) { sub.parse::<i64>().ok() } else { None }
    };
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let (mut hh, mut mm, mut ss, mut ms) = (0i64, 0i64, 0i64, 0i64);
    if b.len() > 10 {
        if b.len() < 19 || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' { return None; }
        hh = num(11, 13)?;
        mm = num(14, 16)?;
        ss = num(17, 19)?;
        if b.len() > 19 && b[19] == b'.' {
            let mut f3: String = s[20..].chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
            while f3.len() < 3 { f3.push('0'); }
            ms = f3.parse::<i64>().unwrap_or(0);
        }
    }
    Some((((days_from_civil(y, mo, d) * 24 + hh) * 60 + mm) * 60 + ss) * 1000 + ms)
}

// JS: parseBody(c.body_json) ?? {}  → parsed object, or {} for null/empty/parse-failure.
fn parse_body(json_cell: Option<&Value>) -> Value {
    let parsed = match json_cell {
        Some(Value::String(s)) if !s.is_empty() => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    if parsed.is_null() { json!({}) } else { parsed }
}

// JS: Date.now() - new Date(updatedAt.replace(' ','T') + 'Z').getTime() > FOURTEEN_DAYS_MS
fn is_stale(updated_at: &str) -> bool {
    match parse_iso_ms(&updated_at.replacen(' ', "T", 1)) {
        Some(ms) => frozen_now_ms() - ms > FOURTEEN_DAYS_MS,
        None => false,
    }
}

// JS Array.prototype.slice(0, end): clamp the (truncated) end into [0, len].
fn slice_end(len: usize, end: &Value) -> usize {
    let n = js_number(Some(end));
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let len_f = len as f64;
    let fin = if n < 0.0 { (len_f + n).max(0.0) } else { n.min(len_f) };
    fin as usize
}

pub fn recommendNextWork(input: &Value, db: &Connection) -> Result<Value, String> {
    // const { limit = 5 } = input ?? {};  (default only when 'limit' is absent; null stays null)
    let limit_val: Value = match input.as_object().and_then(|o| o.get("limit")) {
        Some(v) => v.clone(),
        None => json!(5),
    };

    let cards = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, body_json, priority, parent_slug, updated_at, \
         COALESCE(driver_intent, 'auto') AS driver_intent \
         FROM cards WHERE status = 'backlog' AND slug NOT LIKE 'EPIC-%' ORDER BY updated_at DESC",
        &[],
    )?;
    let cards = cards.as_array().cloned().unwrap_or_default();

    // per-EPIC progress map (only backlog EPICs)
    let mut epic_progress: HashMap<String, f64> = HashMap::new();
    let epics = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug LIKE 'EPIC-%' AND status='backlog'", &[])?;
    for e in epics.as_array().cloned().unwrap_or_default() {
        let eslug = e.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let counts = crate::db_json::query_rows(
            db,
            "SELECT COUNT(*) AS total, SUM(CASE WHEN status='done' THEN 1 ELSE 0 END) AS done FROM cards WHERE parent_slug = ?",
            &[json!(eslug)],
        )?;
        let row = counts.as_array().and_then(|a| a.first().cloned()).unwrap_or(json!({}));
        let total = row.get("total").and_then(Value::as_f64).unwrap_or(0.0);
        let done = row.get("done").and_then(Value::as_f64).unwrap_or(0.0);
        let pp = if total == 0.0 { 0.0 } else { (done / total) * 100.0 };
        epic_progress.insert(eslug, pp);
    }

    // open contracts by card_slug
    let mut open_contracts: HashSet<String> = HashSet::new();
    let contracts = crate::db_json::query_rows(
        db,
        "SELECT card_slug FROM contracts WHERE status IN ('proposed', 'impl', 'tl_approved') AND COALESCE(superseded_by, '') = ''",
        &[],
    )?;
    for r in contracts.as_array().cloned().unwrap_or_default() {
        if let Some(s) = r.get("card_slug").and_then(Value::as_str) { open_contracts.insert(s.to_string()); }
    }

    let mut scored: Vec<Value> = Vec::new();
    for c in &cards {
        let body = parse_body(c.get("body_json"));
        let slug_str = c.get("slug").and_then(Value::as_str).unwrap_or("");
        let parent_str = c.get("parent_slug").and_then(Value::as_str).unwrap_or("");
        let parent_truthy = truthy(c.get("parent_slug"));
        let driver_intent = c.get("driver_intent").and_then(Value::as_str).unwrap_or("");

        let mut reasoning: Vec<Value> = Vec::new();
        let mut score: i64 = 0;

        if !parent_truthy && !open_contracts.contains(slug_str) {
            score += 5;
            reasoning.push(json!("free (no parent, no open contract)"));
        }
        // (c.priority ?? 0) >= 5
        let prio = nz(c.get("priority")).cloned().unwrap_or(json!(0));
        if js_number(Some(&prio)) >= 5.0 {
            score += 5;
            reasoning.push(json!("priority >= 5 (already user-prioritized)"));
        }
        if parent_truthy {
            if let Some(pp) = epic_progress.get(parent_str) {
                if *pp >= 70.0 {
                    score += 4;
                    reasoning.push(json!(format!("parent EPIC {}% done \u{2014} close to wrap", js_round(*pp) as i64)));
                }
            }
        }
        if body.get("files_touched_proposed").and_then(Value::as_array).map_or(false, |a| !a.is_empty()) {
            score += 3;
            reasoning.push(json!("has files_touched_proposed"));
        }
        if driver_intent == "play" {
            score += 3;
            reasoning.push(json!("driver_intent=play (explicit operator signal)"));
        }
        if let Some(acc) = body.get("acceptance").and_then(Value::as_array) {
            if !acc.is_empty() {
                score += 2;
                reasoning.push(json!(format!("{} acceptance criteria", acc.len())));
            }
        }
        if let Some(evals) = body.get("ai_evaluations").and_then(Value::as_array) {
            if !evals.is_empty() {
                let last_rec = evals.last().and_then(|e| e.get("recommendation")).and_then(Value::as_str);
                match last_rec {
                    Some("ARCHIVE") | Some("DEFER") => {
                        score -= 5;
                        reasoning.push(json!(format!("AI last said {} \u{2014} deprioritize", last_rec.unwrap())));
                    }
                    Some("KEEP") => {
                        score += 2;
                        reasoning.push(json!("AI said KEEP"));
                    }
                    _ => {}
                }
            }
        }
        if let Some(ua) = c.get("updated_at").and_then(Value::as_str) {
            if is_stale(ua) {
                score -= 3;
                reasoning.push(json!("stale (>14d untouched)"));
            }
        }
        if truthy(body.get("deferred_until")) {
            if let Some(ms) = body.get("deferred_until").and_then(Value::as_str).and_then(parse_iso_ms) {
                if ms > frozen_now_ms() {
                    score -= 3;
                    reasoning.push(json!("body.deferred_until is in the future"));
                }
            }
        }
        if driver_intent == "paused" || driver_intent == "stopped" {
            score -= 2;
            reasoning.push(json!(format!("driver_intent={}", driver_intent)));
        }

        let mut rec = Map::new();
        rec.insert("slug".into(), c.get("slug").cloned().unwrap_or(Value::Null));
        rec.insert("title".into(), c.get("title").cloned().unwrap_or(Value::Null));
        rec.insert("parent_slug".into(), c.get("parent_slug").cloned().unwrap_or(Value::Null));
        rec.insert("score".into(), json!(score));
        rec.insert("reasoning".into(), Value::Array(reasoning));
        scored.push(Value::Object(rec));
    }

    // scored.sort((a, b) => b.score - a.score) — stable descending by score.
    scored.sort_by(|a, b| {
        let sa = a.get("score").and_then(Value::as_i64).unwrap_or(0);
        let sb = b.get("score").and_then(Value::as_i64).unwrap_or(0);
        sb.cmp(&sa)
    });

    let end = slice_end(scored.len(), &limit_val);
    let recs: Vec<Value> = scored.into_iter().take(end).collect();

    let mut out = Map::new();
    out.insert("ran_at".into(), json!(crate::clock::now_iso(db)));
    out.insert("recommendations".into(), Value::Array(recs));
    out.insert("total_evaluated".into(), json!(cards.len() as i64));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT, priority INTEGER, parent_slug TEXT, updated_at TEXT, driver_intent TEXT); \
             CREATE TABLE contracts (card_slug TEXT, status TEXT, superseded_by TEXT);",
        ).unwrap();
        let ins = |c: &Connection, slug: &str, status: &str, body: Option<&str>, prio: Option<i64>, parent: Option<&str>, upd: &str, di: Option<&str>| {
            c.execute(
                "INSERT INTO cards (slug,title,status,body_json,priority,parent_slug,updated_at,driver_intent) VALUES (?1,?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![slug, status, body, prio, parent, upd, di],
            ).unwrap();
        };
        // candidates (backlog, non-EPIC)
        ins(&c, "A-FREE-PRIO", "backlog", Some("{\"acceptance\":[\"ac1\",\"ac2\"],\"files_touched_proposed\":[\"a.ts\"]}"), Some(5), None, "2026-01-01 00:00:00", Some("play"));
        ins(&c, "B-EPIC-CHILD", "backlog", Some("{}"), Some(0), Some("EPIC-NEAR"), "2026-01-01 00:00:00", Some("auto"));
        ins(&c, "C-STALE-ARCHIVE", "backlog", Some("{\"ai_evaluations\":[{\"recommendation\":\"DEFER\"},{\"recommendation\":\"ARCHIVE\"}]}"), Some(5), None, "2025-12-01 00:00:00", Some("paused"));
        ins(&c, "D-DEFER-KEEP", "backlog", Some("{\"deferred_until\":\"2026-06-01T00:00:00.000Z\",\"ai_evaluations\":[{\"recommendation\":\"KEEP\"}]}"), None, None, "2026-01-01 00:00:00", Some("stopped"));
        ins(&c, "E-EPIC-FAR-CHILD", "backlog", Some("{}"), Some(0), Some("EPIC-FAR"), "2026-01-01 00:00:00", None);
        ins(&c, "F-PARENT-DONE-EPIC", "backlog", Some("{}"), None, Some("EPIC-DONE"), "2026-01-01 00:00:00", Some("auto"));
        ins(&c, "G-DEFER-PAST", "backlog", Some("{\"deferred_until\":\"2025-01-01T00:00:00.000Z\"}"), None, None, "2026-01-01 00:00:00", Some("auto"));
        // EPICs
        ins(&c, "EPIC-NEAR", "backlog", None, None, None, "2026-01-01 00:00:00", Some("auto"));
        ins(&c, "EPIC-FAR", "backlog", None, None, None, "2026-01-01 00:00:00", Some("auto"));
        ins(&c, "EPIC-EMPTY", "backlog", None, None, None, "2026-01-01 00:00:00", Some("auto"));
        ins(&c, "EPIC-DONE", "done", None, None, None, "2026-01-01 00:00:00", Some("auto"));
        // EPIC-NEAR children: total 9, done 7 → 77.7% → round 78
        ins(&c, "NEAR-DOING", "doing", None, None, Some("EPIC-NEAR"), "2026-01-01 00:00:00", Some("auto"));
        for i in 0..7 { ins(&c, &format!("NEAR-DONE-{i}"), "done", None, None, Some("EPIC-NEAR"), "2026-01-01 00:00:00", Some("auto")); }
        // EPIC-FAR children: total 2, done 1 → 50%
        ins(&c, "FAR-DONE-1", "done", None, None, Some("EPIC-FAR"), "2026-01-01 00:00:00", Some("auto"));
        // contracts
        c.execute("INSERT INTO contracts (card_slug,status,superseded_by) VALUES ('A-FREE-PRIO','proposed','SOME-NEWER')", []).unwrap();
        c.execute("INSERT INTO contracts (card_slug,status,superseded_by) VALUES ('C-STALE-ARCHIVE','impl',NULL)", []).unwrap();
        c.execute("INSERT INTO contracts (card_slug,status,superseded_by) VALUES ('D-DEFER-KEEP','done',NULL)", []).unwrap();
        c
    }

    fn find<'a>(recs: &'a [Value], slug: &str) -> &'a Value {
        recs.iter().find(|r| r["slug"] == json!(slug)).unwrap()
    }

    #[test]
    fn scores_and_orders() {
        let c = seeded();
        let r = recommendNextWork(&json!({ "limit": 100 }), &c).unwrap();
        assert_eq!(r["ran_at"], json!("2026-01-01T00:00:00.000Z"));
        assert_eq!(r["total_evaluated"], json!(7));
        let recs = r["recommendations"].as_array().unwrap();
        assert_eq!(recs.len(), 7);
        // A is the unique top (18), C the unique bottom (-5)
        assert_eq!(recs.first().unwrap()["slug"], json!("A-FREE-PRIO"));
        assert_eq!(recs.last().unwrap()["slug"], json!("C-STALE-ARCHIVE"));

        let a = find(recs, "A-FREE-PRIO");
        assert_eq!(a["score"], json!(18));
        assert_eq!(a["reasoning"], json!([
            "free (no parent, no open contract)",
            "priority >= 5 (already user-prioritized)",
            "has files_touched_proposed",
            "driver_intent=play (explicit operator signal)",
            "2 acceptance criteria"
        ]));

        let b = find(recs, "B-EPIC-CHILD");
        assert_eq!(b["score"], json!(4));
        assert_eq!(b["reasoning"], json!(["parent EPIC 78% done \u{2014} close to wrap"]));

        let cc = find(recs, "C-STALE-ARCHIVE");
        assert_eq!(cc["score"], json!(-5));
        assert_eq!(cc["reasoning"], json!([
            "priority >= 5 (already user-prioritized)",
            "AI last said ARCHIVE \u{2014} deprioritize",
            "stale (>14d untouched)",
            "driver_intent=paused"
        ]));

        let d = find(recs, "D-DEFER-KEEP");
        assert_eq!(d["score"], json!(2));
        assert_eq!(d["reasoning"], json!([
            "free (no parent, no open contract)",
            "AI said KEEP",
            "body.deferred_until is in the future",
            "driver_intent=stopped"
        ]));

        assert_eq!(find(recs, "E-EPIC-FAR-CHILD")["score"], json!(0));
        assert_eq!(find(recs, "F-PARENT-DONE-EPIC")["score"], json!(0));
        let g = find(recs, "G-DEFER-PAST");
        assert_eq!(g["score"], json!(5)); // deferred_until in the past → no penalty
    }

    #[test]
    fn respects_limit_and_empty() {
        let c = seeded();
        let r = recommendNextWork(&json!({ "limit": 2 }), &c).unwrap();
        assert_eq!(r["recommendations"].as_array().unwrap().len(), 2);
        assert_eq!(r["total_evaluated"], json!(7));
        // default limit = 5
        let r5 = recommendNextWork(&json!({}), &c).unwrap();
        assert_eq!(r5["recommendations"].as_array().unwrap().len(), 5);

        let empty = Connection::open_in_memory().unwrap();
        empty.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, body_json TEXT, priority INTEGER, parent_slug TEXT, updated_at TEXT, driver_intent TEXT); \
             CREATE TABLE contracts (card_slug TEXT, status TEXT, superseded_by TEXT);",
        ).unwrap();
        let re = recommendNextWork(&json!({}), &empty).unwrap();
        assert_eq!(re["recommendations"], json!([]));
        assert_eq!(re["total_evaluated"], json!(0));
    }
}
