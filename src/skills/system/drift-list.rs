//! drift-list.rs — twin of skills/system/drift-list.mjs (db READ lane, cross-db driver_ticks via
//! ATTACH). NAMES ARE CONTRACT: `driftList` (default). Unified feed of operator-attention items:
//! pending diffs awaiting approval, cards whose latest driver tick failed (drift), and cards stuck
//! in a 3+ consecutive same-action failure loop (stuck) — minus any covered by a dismissed
//! driver-tick-failed notification. Ranked severity error>warn>info, then age desc, capped.
//! Frozen-clock lane: JS `Date.now()` is pinned to 2026-01-01T00:00:00.000Z (1767225600000 ms).
use rusqlite::Connection;
use serde_json::{json, Map, Value};


// Standard helpers (playbook, copied verbatim — only the ones this twin needs).
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
// JS Math.round: nearest integer, ties toward +Infinity (floor(x+0.5) without the float-add bug).
fn js_round(x: f64) -> f64 { if !x.is_finite() { return x; } let f = x.floor(); if x - f < 0.5 { f } else { f + 1.0 } }

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
// Parse 'YYYY-MM-DD[T| ]HH:MM:SS[.sss][Z]' as a UTC instant → epoch ms (mirrors Date.parse of an
// already-normalized 'T'+'Z' string). Malformed → None (JS NaN, where `!Number.isFinite` is true).
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

// JS: function ageSeconds(ts) { if (!ts) return 0; const parsed = ts.includes('T') ? Date.parse(ts)
//   : Date.parse(ts.replace(' ', 'T') + 'Z'); if (!Number.isFinite(parsed)) return 0;
//   return Math.max(0, Math.round((Date.now() - parsed) / 1000)); }
fn age_seconds(ts: Option<&Value>) -> i64 {
    let s = match ts { Some(Value::String(s)) if !s.is_empty() => s.as_str(), _ => return 0 };
    let parsed = if s.contains('T') { parse_iso_ms(s) } else { parse_iso_ms(&format!("{}Z", s.replacen(' ', "T", 1))) };
    let parsed_ms = match parsed { Some(ms) => ms, None => return 0 };
    let diff = js_round((crate::clock::now_ms() - parsed_ms) as f64 / 1000.0);
    if diff < 0.0 { 0 } else { diff as i64 }
}

pub fn driftList(input: &Value, db: &Connection) -> Result<Value, String> {
    // cap = Math.min(Math.max(1, Number(limit) || 100), 500)
    // (destructure default `limit = 100` fires only for a MISSING key; an explicit null still
    // flows through Number(null)=0 -> ||100, so both paths land on the same final 100.)
    let limit_raw: f64 = match input.get("limit") { None => 100.0, Some(v) => js_number(Some(v)) };
    let limit_or = if limit_raw == 0.0 || limit_raw.is_nan() { 100.0 } else { limit_raw };
    let cap_f = limit_or.max(1.0).min(500.0);
    let cap: i64 = cap_f.trunc() as i64; // JS ToIntegerOrInfinity truncation (LIMIT + final .slice)

    // `filter = 'all'` default likewise fires only for a MISSING key: an explicit `filter: null`
    // (or any non-string value) never strictly-equals 'all'/'diff'/'drift'/'stuck', so every
    // want* flag goes false and the skill returns [] — that quirk is intentional, transcribe it.
    let filter: Option<&str> = match input.get("filter") {
        None => Some("all"),
        Some(Value::String(s)) => Some(s.as_str()),
        _ => None,
    };
    let want_diff = filter == Some("all") || filter == Some("diff");
    let want_drift = filter == Some("all") || filter == Some("drift");
    let want_stuck = filter == Some("all") || filter == Some("stuck");

    let mut items: Vec<Value> = Vec::new();

    // ── pending diffs ──────────────────────────────────────────────────────
    if want_diff {
        let sql = "SELECT id, card_slug, file_path, proposed_by, proposed_at, status FROM pending_diffs WHERE decided_at IS NULL ORDER BY proposed_at DESC LIMIT ?";
        if let Ok(rows) = crate::db_json::query_rows(db, sql, &[json!(cap)]) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let mut o = Map::new();
                o.insert("type".into(), json!("diff"));
                o.insert("id".into(), json!(format!("diff-{}", tmpl(r.get("id")))));
                o.insert("slug".into(), r.get("card_slug").cloned().unwrap_or(Value::Null));
                o.insert("age_seconds".into(), json!(age_seconds(r.get("proposed_at"))));
                o.insert("severity".into(), json!("info"));
                o.insert("summary".into(), json!(format!("{} proposed a patch on {}", tmpl(r.get("proposed_by")), tmpl(r.get("file_path")))));
                o.insert("last_error".into(), Value::Null);
                o.insert("last_tick_id".into(), Value::Null);
                o.insert("dispatched_action".into(), Value::Null);
                o.insert("source_id".into(), r.get("id").cloned().unwrap_or(Value::Null));
                o.insert("hint".into(), json!("Operator reviews + approve / reject. Approve dispatches diff.apply."));
                items.push(Value::Object(o));
            }
        }
    }

    // ── failed-last-tick drifts (+ stuck retry-loop detection) ─────────────
    if want_drift || want_stuck {
        let sql = "WITH latest AS (\n  SELECT t.*, ROW_NUMBER() OVER (PARTITION BY t.card_slug ORDER BY t.id DESC) AS rn\n  FROM brain.driver_ticks t\n  WHERE t.card_slug IS NOT NULL\n)\nSELECT id, started_at, card_slug, decided_agent, decided_action,\n       decided_reason, outcome, error_message\nFROM latest\nWHERE rn = 1 AND outcome = 'failed'\nORDER BY id DESC\nLIMIT ?";
        let failed_ticks: Vec<Value> = crate::db_json::query_rows(db, sql, &[json!(cap)]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();

        for t in &failed_ticks {
            // Count consecutive failed ticks on the same decided_action — stuck signal.
            let mut consecutive: i64 = 1;
            if let Ok(recent) = crate::db_json::query_rows(
                db,
                "SELECT outcome, decided_action FROM brain.driver_ticks WHERE card_slug = ? AND decided_action IS NOT NULL ORDER BY id DESC LIMIT 10",
                &[t.get("card_slug").cloned().unwrap_or(Value::Null)],
            ) {
                if let Some(arr) = recent.as_array() {
                    for (i, row) in arr.iter().enumerate() {
                        let outcome_failed = row.get("outcome").and_then(|v| v.as_str()) == Some("failed");
                        let same_action = row.get("decided_action") == t.get("decided_action");
                        if outcome_failed && same_action {
                            consecutive = (i as i64) + 1;
                        } else {
                            break;
                        }
                    }
                }
            }

            let is_stuck = consecutive >= 3;
            if is_stuck && !want_stuck { continue; }
            if !is_stuck && !want_drift { continue; }

            // Skip if a dismissed notification covers this failure already.
            let decided_action_param = nz(t.get("decided_action")).cloned().unwrap_or(json!(""));
            let mut covered_by_dismissed = false;
            if let Ok(cov_rows) = crate::db_json::query_rows(
                db,
                "SELECT 1 FROM notifications WHERE related_card_slug = ? AND kind = 'driver-tick-failed' AND dismissed_at IS NOT NULL AND payload_json LIKE '%' || ? || '%' LIMIT 1",
                &[t.get("card_slug").cloned().unwrap_or(Value::Null), decided_action_param],
            ) {
                covered_by_dismissed = cov_rows.as_array().map_or(false, |a| !a.is_empty());
            }
            if covered_by_dismissed { continue; }

            let err_brief: String = match t.get("error_message") {
                Some(Value::String(s)) => s.chars().take(200).collect(),
                _ => String::new(),
            };

            let agent_disp = match nz(t.get("decided_agent")) { Some(v) => tmpl(Some(v)), None => "?".to_string() };
            let action_disp = match nz(t.get("decided_action")) { Some(v) => tmpl(Some(v)), None => "?".to_string() };
            let mut summary = format!("{} \u{2192} {} failed", agent_disp, action_disp);
            if is_stuck { summary.push_str(&format!(" \u{b7} {} consecutive", consecutive)); }

            let hint = if is_stuck {
                "Repeating failure \u{2014} operator likely needs to redraft the contract or restart the card."
            } else {
                "Operator picks resolution path: redraft / amend / restart / acknowledge."
            };

            let mut o = Map::new();
            o.insert("type".into(), json!(if is_stuck { "stuck" } else { "drift" }));
            o.insert("id".into(), json!(format!("tick-{}", tmpl(t.get("id")))));
            o.insert("slug".into(), t.get("card_slug").cloned().unwrap_or(Value::Null));
            o.insert("age_seconds".into(), json!(age_seconds(t.get("started_at"))));
            o.insert("severity".into(), json!(if is_stuck { "error" } else { "warn" }));
            o.insert("summary".into(), json!(summary));
            o.insert("last_error".into(), json!(err_brief));
            o.insert("last_tick_id".into(), t.get("id").cloned().unwrap_or(Value::Null));
            o.insert("dispatched_action".into(), t.get("decided_action").cloned().unwrap_or(Value::Null));
            o.insert("source_id".into(), t.get("id").cloned().unwrap_or(Value::Null));
            o.insert("hint".into(), json!(hint));
            o.insert("decided_agent".into(), t.get("decided_agent").cloned().unwrap_or(Value::Null));
            o.insert("decided_action".into(), t.get("decided_action").cloned().unwrap_or(Value::Null));
            o.insert("decided_reason".into(), t.get("decided_reason").cloned().unwrap_or(Value::Null));
            items.push(Value::Object(o));
        }
    }

    // Rank: severity error > warn > info, then age desc.
    fn sev_rank(v: Option<&Value>) -> i64 {
        match v.and_then(|x| x.as_str()) { Some("error") => 0, Some("warn") => 1, Some("info") => 2, _ => 3 }
    }
    items.sort_by(|a, b| {
        let sev_cmp = sev_rank(a.get("severity")).cmp(&sev_rank(b.get("severity")));
        if sev_cmp != std::cmp::Ordering::Equal { return sev_cmp; }
        let bage = b.get("age_seconds").and_then(|v| v.as_i64()).unwrap_or(0);
        let aage = a.get("age_seconds").and_then(|v| v.as_i64()).unwrap_or(0);
        bage.cmp(&aage)
    });

    let out: Vec<Value> = items.into_iter().take(cap.max(0) as usize).collect();
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE pending_diffs (id INTEGER PRIMARY KEY, card_slug TEXT, file_path TEXT, proposed_by TEXT, proposed_at TEXT, decided_at TEXT, status TEXT); \
             CREATE TABLE notifications (id INTEGER PRIMARY KEY, kind TEXT, related_card_slug TEXT, dismissed_at TEXT, payload_json TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, error_message TEXT); \
             INSERT INTO pending_diffs (id,card_slug,file_path,proposed_by,proposed_at,decided_at,status) VALUES \
               (1,'CARD-A','src/foo.rs','dev','2025-12-31 22:00:00',NULL,'pending'), \
               (2,'CARD-B','src/bar.rs','qa','2025-12-30T12:00:00.000Z',NULL,'pending'), \
               (3,'CARD-C','src/baz.rs','dev','2025-12-20 00:00:00','2025-12-21 00:00:00','approved'); \
             INSERT INTO brain.driver_ticks (id,started_at,card_slug,decided_agent,decided_action,decided_reason,outcome,error_message) VALUES \
               (10,'2025-12-25 00:00:00','STUCK-1','dev','dev.card.update','r1','failed','boom1'), \
               (11,'2025-12-26 00:00:00','STUCK-1','dev','dev.card.update','r2','failed','boom2'), \
               (12,'2025-12-27 00:00:00','STUCK-1','dev','dev.card.update','r3','failed','boom3'), \
               (20,'2025-12-29 00:00:00','DRIFT-1','qa','qa.review','r','success',NULL), \
               (21,'2025-12-30 06:00:00','DRIFT-1','qa','qa.review','r2','failed','boom-drift'), \
               (30,'2025-12-28 00:00:00','NOTFAILED-1','dev','dev.x','r','success',NULL), \
               (40,'2025-12-31 00:00:00','DISMISSED-1','dev','dev.card.delete','r','failed','boom-dismissed'); \
             INSERT INTO notifications (id,kind,related_card_slug,dismissed_at,payload_json) VALUES \
               (100,'driver-tick-failed','DISMISSED-1','2025-12-31 12:00:00','{\"decided_action\":\"dev.card.delete\"}'), \
               (101,'driver-tick-failed','STUCK-1',NULL,'{\"decided_action\":\"dev.card.update\"}'), \
               (102,'driver-tick-failed','DRIFT-1','2025-12-30 10:00:00','{\"decided_action\":\"other.action\"}');",
        ).unwrap();
        c
    }

    #[test]
    fn merges_ranks_and_filters() {
        let c = db();
        let all = driftList(&json!({}), &c).unwrap();
        let rows = all.as_array().unwrap();
        // 2 diffs (CARD-C excluded: decided_at not null) + stuck(STUCK-1) + drift(DRIFT-1);
        // NOTFAILED-1 excluded (latest tick not failed); DISMISSED-1 excluded (dismissed notif covers it).
        assert_eq!(rows.len(), 4);
        // severity error > warn > info
        assert_eq!(rows[0]["type"], json!("stuck"));
        assert_eq!(rows[0]["severity"], json!("error"));
        assert_eq!(rows[1]["type"], json!("drift"));
        assert_eq!(rows[1]["severity"], json!("warn"));
        assert_eq!(rows[2]["type"], json!("diff"));
        assert_eq!(rows[3]["type"], json!("diff"));
        // within info tier, age desc: CARD-B (older proposed_at) before CARD-A
        assert_eq!(rows[2]["slug"], json!("CARD-B"));
        assert_eq!(rows[3]["slug"], json!("CARD-A"));

        let stuck = &rows[0];
        assert_eq!(stuck["id"], json!("tick-12"));
        assert!(stuck["summary"].as_str().unwrap().contains("3 consecutive"));
        assert_eq!(stuck["dispatched_action"], json!("dev.card.update"));

        let drift = &rows[1];
        assert_eq!(drift["id"], json!("tick-21"));
        assert_eq!(drift["last_error"], json!("boom-drift"));

        // filter narrows to a single kind
        let diff_only = driftList(&json!({"filter": "diff"}), &c).unwrap();
        assert_eq!(diff_only.as_array().unwrap().len(), 2);
        assert!(diff_only.as_array().unwrap().iter().all(|r| r["type"] == json!("diff")));

        let stuck_only = driftList(&json!({"filter": "stuck"}), &c).unwrap();
        assert_eq!(stuck_only.as_array().unwrap().len(), 1);
        assert_eq!(stuck_only[0]["type"], json!("stuck"));

        let drift_only = driftList(&json!({"filter": "drift"}), &c).unwrap();
        assert_eq!(drift_only.as_array().unwrap().len(), 1);
        assert_eq!(drift_only[0]["type"], json!("drift"));

        // explicit null filter never strictly-equals any literal -> nothing wanted
        assert_eq!(driftList(&json!({"filter": null}), &c).unwrap(), json!([]));

        // limit=1: per-query LIMIT 1 pre-filters BEFORE stuck/dismissed processing — the
        // top-1-by-id failed tick is DISMISSED-1 (id 40), which then gets dropped by the
        // dismissed-notification check, so only the top-1-by-proposed_at diff (CARD-A) survives.
        let capped = driftList(&json!({"limit": 1}), &c).unwrap();
        assert_eq!(capped.as_array().unwrap().len(), 1);
        assert_eq!(capped[0]["slug"], json!("CARD-A"));
    }

    #[test]
    fn empty_tables_yield_empty_list() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE pending_diffs (id INTEGER PRIMARY KEY, card_slug TEXT, file_path TEXT, proposed_by TEXT, proposed_at TEXT, decided_at TEXT, status TEXT); \
             CREATE TABLE notifications (id INTEGER PRIMARY KEY, kind TEXT, related_card_slug TEXT, dismissed_at TEXT, payload_json TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, error_message TEXT);",
        ).unwrap();
        assert_eq!(driftList(&json!(null), &c).unwrap(), json!([]));
    }
}
