//! code-review-history.rs — twin of skills/codebase/code-review-history.mjs (db READ lane,
//! cross-db ATTACH: judgments.review_snapshots / judgments.review_scores). NAMES ARE CONTRACT:
//! `codeReviewHistory` (default). HISTORY REVIEW axis: the repo health timeline across DONE
//! judgment snapshots, the most-improved/most-regressed files (per-file health series aligned to
//! the timeline), and the repo-average per-axis drift between the first and last snapshot in the
//! window. 100% deterministic — reads the ledger the judge already wrote, no clock/AI involved.
// QUALITY: faithful clone — windowed timeline, first/last-seen mover deltas, ordered axis drift
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

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
// JS number → JSON: integer-valued renders without a decimal point (as i64), else f64.
fn num_to_value(x: f64) -> Value {
    if x == 0.0 { return json!(0i64); } // normalize -0.0 → 0
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9.007_199_254_740_992e15 { json!(x as i64) } else { json!(x) }
}
// JS: num(v) = v == null ? null : Math.round(Number(v))
fn num(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(x) => num_to_value(crate::js_math::js_round(js_number(Some(x)))),
    }
}
// JS: parse(s) = try { s ? JSON.parse(s) : null } catch { null } — s is the axes_json TEXT column
// (always a plain string or NULL here, so the JS `s ? ... : null` truthiness check collapses to
// "non-empty string").
fn parse_axes(v: Option<&Value>) -> Option<Value> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => serde_json::from_str::<Value>(s).ok(),
        _ => None,
    }
}
// JS `Object.keys(a)` for the parsed axes object; a non-object payload has no axis keys to walk.
fn axis_keys(v: &Value) -> Vec<String> {
    match v { Value::Object(m) => m.keys().cloned().collect(), _ => Vec::new() }
}

// per-snapshot axis averages, as a Vec preserving Map insertion order (first `.set()` wins position).
fn axis_avg(db: &Connection, snap_id: &Value) -> Vec<(String, f64)> {
    let rows: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT axes_json FROM judgments.review_scores WHERE unit_kind='file' AND snapshot_id=?",
        &[snap_id.clone()],
    ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut order: Vec<String> = Vec::new();
    let mut sum: HashMap<String, f64> = HashMap::new();
    let mut cnt: HashMap<String, i64> = HashMap::new();
    for r in &rows {
        let axes = match parse_axes(r.get("axes_json")) { Some(a) => a, None => continue };
        for k in axis_keys(&axes) {
            let sc = axes.get(&k).and_then(|v| v.get("score")).filter(|v| !v.is_null());
            let sc = match sc { Some(v) => v, None => continue };
            if !sum.contains_key(&k) { order.push(k.clone()); }
            *sum.entry(k.clone()).or_insert(0.0) += js_number(Some(sc));
            *cnt.entry(k.clone()).or_insert(0) += 1;
        }
    }
    order.into_iter().map(|k| { let avg = sum[&k] / cnt[&k] as f64; (k, avg) }).collect()
}

pub fn codeReviewHistory(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('code-review-history','db') when no db — the db lane always
    // injects one, so the guard is unreachable and not transcribed (playbook rule 9).
    let limit_raw = match input.get("limit").filter(|v| !v.is_null()) { Some(v) => js_number(Some(v)), None => 40.0 };
    let limit = limit_raw.max(2.0).min(200.0);
    let top_raw = match input.get("top").filter(|v| !v.is_null()) { Some(v) => js_number(Some(v)), None => 8.0 };
    let top = top_raw.max(1.0).min(30.0);

    // the repo timeline: avg health/score per DONE judgment snapshot (each rebuild's review pass)
    let mut snaps: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT s.id, s.created_at, \
                COUNT(*) AS files, AVG(sc.health) AS avg_health, AVG(sc.score) AS avg_score \
         FROM judgments.review_snapshots s \
         JOIN judgments.review_scores sc ON sc.snapshot_id = s.id AND sc.unit_kind = 'file' \
         WHERE s.status = 'done' \
         GROUP BY s.id ORDER BY s.id ASC",
        &[],
    ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    // `.slice(-limit)` — the most recent window (limit is already clamped >= 2, finite)
    let limit_n = limit as usize;
    if snaps.len() > limit_n { snaps = snaps[snaps.len() - limit_n..].to_vec(); }

    let timeline: Vec<Value> = snaps.iter().map(|s| json!({
        "id": s.get("id").cloned().unwrap_or(Value::Null),
        "at": s.get("created_at").cloned().unwrap_or(Value::Null),
        "files": s.get("files").cloned().unwrap_or(Value::Null),
        "avg_health": num(s.get("avg_health")),
        "avg_score": num(s.get("avg_score")),
    })).collect();

    if snaps.len() < 2 {
        let mut out = Map::new();
        out.insert("snapshot_count".into(), json!(snaps.len() as i64));
        out.insert("file_count".into(), snaps.last().and_then(|s| s.get("files")).cloned().unwrap_or(json!(0)));
        out.insert("timeline".into(), json!(timeline));
        out.insert("movers".into(), json!({ "improved": [], "regressed": [] }));
        out.insert("axes_drift".into(), json!([]));
        out.insert("insufficient".into(), json!(true));
        return Ok(Value::Object(out));
    }

    let ids: Vec<Value> = snaps.iter().map(|s| s.get("id").cloned().unwrap_or(Value::Null)).collect();
    let mut id_index: HashMap<i64, usize> = HashMap::new();
    for (i, id) in ids.iter().enumerate() { if let Some(n) = id.as_i64() { id_index.insert(n, i); } }

    // per-file health series across the window → movers (first-seen vs latest health)
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let rows_sql = format!(
        "SELECT path, snapshot_id, health FROM judgments.review_scores WHERE unit_kind='file' AND snapshot_id IN ({placeholders}) ORDER BY path ASC, snapshot_id ASC"
    );
    let rows: Vec<Value> = crate::db_json::query_rows(db, &rows_sql, &ids)
        .ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();

    let mut path_order: Vec<String> = Vec::new();
    let mut path_idx: HashMap<String, usize> = HashMap::new();
    let mut series_list: Vec<Vec<Value>> = Vec::new();
    for r in &rows {
        let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let pos = *path_idx.entry(path.clone()).or_insert_with(|| {
            path_order.push(path.clone());
            series_list.push(vec![Value::Null; ids.len()]);
            path_order.len() - 1
        });
        if let Some(sid) = r.get("snapshot_id").and_then(|v| v.as_i64()) {
            if let Some(&idx) = id_index.get(&sid) { series_list[pos][idx] = num(r.get("health")); }
        }
    }

    struct Mover { path: String, from: Value, to: Value, delta: Value, delta_f: f64, series: Vec<Value> }
    let mut movers: Vec<Mover> = Vec::new();
    for (i, path) in path_order.iter().enumerate() {
        let series = &series_list[i];
        let present: Vec<&Value> = series.iter().filter(|v| !v.is_null()).collect();
        if present.len() < 2 { continue; }
        let from_v = present[0].clone();
        let to_v = present[present.len() - 1].clone();
        let delta_f = to_v.as_f64().unwrap_or(0.0) - from_v.as_f64().unwrap_or(0.0);
        movers.push(Mover { path: path.clone(), from: from_v, to: to_v, delta: num_to_value(delta_f), delta_f, series: series.clone() });
    }
    let mover_json = |m: &Mover| json!({ "path": m.path, "from": m.from, "to": m.to, "delta": m.delta, "series": m.series });

    let mut improved: Vec<&Mover> = movers.iter().filter(|m| m.delta_f > 0.0).collect();
    improved.sort_by(|a, b| b.delta_f.partial_cmp(&a.delta_f).unwrap());
    improved.truncate(top as usize);
    let improved_json: Vec<Value> = improved.iter().map(|&m| mover_json(m)).collect();

    let mut regressed: Vec<&Mover> = movers.iter().filter(|m| m.delta_f < 0.0).collect();
    regressed.sort_by(|a, b| a.delta_f.partial_cmp(&b.delta_f).unwrap());
    regressed.truncate(top as usize);
    let regressed_json: Vec<Value> = regressed.iter().map(|&m| mover_json(m)).collect();

    // repo-average per-axis drift: first window snapshot → last (which axes carried the repo up or down)
    let first_a = axis_avg(db, &ids[0]);
    let last_a = axis_avg(db, &ids[ids.len() - 1]);
    let first_map: HashMap<&str, f64> = first_a.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let last_map: HashMap<&str, f64> = last_a.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let mut axis_order: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (k, _) in first_a.iter().chain(last_a.iter()) { if seen.insert(k.clone()) { axis_order.push(k.clone()); } }

    struct AxisDrift { axis: String, from: Value, to: Value, delta: Value, delta_f: f64 }
    let mut axes_drift: Vec<AxisDrift> = Vec::new();
    for axis in &axis_order {
        let from_opt = first_map.get(axis.as_str());
        let to_opt = last_map.get(axis.as_str());
        if let (Some(&fv), Some(&tv)) = (from_opt, to_opt) {
            let from_r = crate::js_math::js_round(fv);
            let to_r = crate::js_math::js_round(tv);
            axes_drift.push(AxisDrift {
                axis: axis.clone(),
                from: num_to_value(from_r),
                to: num_to_value(to_r),
                delta: num_to_value(to_r - from_r),
                delta_f: to_r - from_r,
            });
        }
    }
    axes_drift.sort_by(|a, b| b.delta_f.abs().partial_cmp(&a.delta_f.abs()).unwrap());
    let axes_drift_json: Vec<Value> = axes_drift.iter()
        .map(|d| json!({ "axis": d.axis, "from": d.from, "to": d.to, "delta": d.delta }))
        .collect();

    let mut out = Map::new();
    out.insert("snapshot_count".into(), json!(snaps.len() as i64));
    out.insert("file_count".into(), snaps.last().and_then(|s| s.get("files")).cloned().unwrap_or(Value::Null));
    out.insert("timeline".into(), json!(timeline));
    out.insert("movers".into(), json!({ "improved": improved_json, "regressed": regressed_json }));
    out.insert("axes_drift".into(), json!(axes_drift_json));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS judgments; \
             CREATE TABLE judgments.review_snapshots (id INTEGER PRIMARY KEY, created_at TEXT, status TEXT); \
             CREATE TABLE judgments.review_scores (snapshot_id INTEGER, unit_kind TEXT, path TEXT, health REAL, score REAL, axes_json TEXT);",
        ).unwrap();
        c
    }
    fn snap(c: &Connection, id: i64, at: &str, status: &str) {
        c.execute("INSERT INTO judgments.review_snapshots (id, created_at, status) VALUES (?,?,?)", rusqlite::params![id, at, status]).unwrap();
    }
    fn score(c: &Connection, snap_id: i64, path: &str, health: f64, axes: Option<&str>) {
        c.execute(
            "INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, health, score, axes_json) VALUES (?, 'file', ?, ?, ?, ?)",
            rusqlite::params![snap_id, path, health, health, axes],
        ).unwrap();
    }

    #[test]
    fn timeline_movers_and_axis_drift() {
        let c = setup();
        snap(&c, 1, "2026-06-01 10:00:00", "done");
        snap(&c, 2, "2026-07-01 10:00:00", "done");
        // a.ts climbs 60→90 (+30, most improved); b.ts falls 80→50 (−30, most regressed)
        score(&c, 1, "a.ts", 60.0, Some(r#"{"structure":{"score":50},"complexity":{"score":70}}"#));
        score(&c, 2, "a.ts", 90.0, Some(r#"{"structure":{"score":80},"complexity":{"score":90}}"#));
        score(&c, 1, "b.ts", 80.0, Some(r#"{"structure":{"score":90},"complexity":{"score":80}}"#));
        score(&c, 2, "b.ts", 50.0, Some(r#"{"structure":{"score":40},"complexity":{"score":60}}"#));

        let r = codeReviewHistory(&json!({}), &c).unwrap();
        assert_eq!(r["snapshot_count"], json!(2));
        assert_eq!(r["file_count"], json!(2));
        assert_eq!(r["timeline"][0]["avg_health"], json!(70)); // (60+80)/2
        assert_eq!(r["timeline"][1]["avg_health"], json!(70)); // (90+50)/2
        assert_eq!(r["movers"]["improved"][0]["path"], json!("a.ts"));
        assert_eq!(r["movers"]["improved"][0]["delta"], json!(30));
        assert_eq!(r["movers"]["improved"][0]["series"], json!([60, 90]));
        assert_eq!(r["movers"]["regressed"][0]["path"], json!("b.ts"));
        assert_eq!(r["movers"]["regressed"][0]["delta"], json!(-30));
        let structure = r["axes_drift"].as_array().unwrap().iter().find(|d| d["axis"] == json!("structure")).unwrap();
        // structure repo-avg: (50+90)/2=70 → (80+40)/2=60, drift −10
        assert_eq!(structure["from"], json!(70));
        assert_eq!(structure["to"], json!(60));
        assert_eq!(structure["delta"], json!(-10));
    }

    #[test]
    fn fewer_than_two_snapshots_is_insufficient() {
        let c = setup();
        snap(&c, 1, "2026-07-01 10:00:00", "done");
        score(&c, 1, "a.ts", 80.0, None);
        let r = codeReviewHistory(&json!({}), &c).unwrap();
        assert_eq!(r["snapshot_count"], json!(1));
        assert_eq!(r["insufficient"], json!(true));
        assert_eq!(r["movers"]["improved"], json!([]));
        assert_eq!(r["file_count"], json!(1));
    }

    #[test]
    fn non_done_snapshots_excluded_and_limit_windows_and_top_caps() {
        let c = setup();
        snap(&c, 1, "2026-01-01 10:00:00", "running"); // excluded — not 'done'
        snap(&c, 2, "2026-02-01 10:00:00", "done");
        snap(&c, 3, "2026-03-01 10:00:00", "done");
        snap(&c, 4, "2026-04-01 10:00:00", "done");
        score(&c, 1, "z.ts", 10.0, None);
        score(&c, 2, "a.ts", 50.0, None);
        score(&c, 2, "b.ts", 50.0, None);
        score(&c, 2, "c.ts", 50.0, None);
        score(&c, 3, "a.ts", 55.0, None);
        score(&c, 3, "b.ts", 45.0, None);
        score(&c, 3, "c.ts", 60.0, None);
        score(&c, 4, "a.ts", 65.0, None);
        score(&c, 4, "b.ts", 30.0, None);
        score(&c, 4, "c.ts", 62.0, None);

        // limit=2 keeps only the two most recent DONE snapshots (ids 3 and 4)
        let r = codeReviewHistory(&json!({ "limit": 2, "top": 1 }), &c).unwrap();
        assert_eq!(r["snapshot_count"], json!(2));
        assert_eq!(r["timeline"][0]["id"], json!(3));
        assert_eq!(r["timeline"][1]["id"], json!(4));
        // a.ts: 55→65 (+10), c.ts: 60→62 (+2), b.ts: 45→30 (-15); top=1 caps each list to 1
        assert_eq!(r["movers"]["improved"].as_array().unwrap().len(), 1);
        assert_eq!(r["movers"]["improved"][0]["path"], json!("a.ts"));
        assert_eq!(r["movers"]["regressed"].as_array().unwrap().len(), 1);
        assert_eq!(r["movers"]["regressed"][0]["path"], json!("b.ts"));

        // no axes_json anywhere → axes_drift stays empty
        assert_eq!(r["axes_drift"], json!([]));
    }

    #[test]
    fn axis_present_only_on_one_side_is_dropped() {
        let c = setup();
        snap(&c, 1, "2026-06-01 10:00:00", "done");
        snap(&c, 2, "2026-07-01 10:00:00", "done");
        score(&c, 1, "a.ts", 60.0, Some(r#"{"only_first":{"score":40}}"#));
        score(&c, 2, "a.ts", 90.0, Some(r#"{"only_last":{"score":90}}"#));
        let r = codeReviewHistory(&json!({}), &c).unwrap();
        assert_eq!(r["axes_drift"], json!([]));
    }
}
