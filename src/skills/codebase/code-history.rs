//! code-history.rs — twin of skills/codebase/code-history.mjs (db READ lane). NAMES ARE CONTRACT:
//! `codeHistory` (default). Architecture-over-time from the append-only code_digests snapshots +
//! entry-point drift. Returns {origin, count, points[], trend{now,month,quarter}, incident|null}.
// QUALITY: faithful clone — timeline w/ per-step deltas, month/quarter trend, structural incident diff
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(x)) => x.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS String(v)
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(x) => x.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
// = JS Number(v) for a present value
fn js_number_val(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => if *b { 1.0 } else { 0.0 },
        Value::Number(x) => x.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        _ => f64::NAN,
    }
}
// JS number → JSON: integer-valued renders without a decimal point (as i64), else f64.
fn num_to_value(x: f64) -> Value {
    if x == 0.0 { return json!(0i64); } // normalize -0.0 → 0
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9.007_199_254_740_992e15 { json!(x as i64) } else { json!(x) }
}
// = JS `+(x).toFixed(digits)`. All call-sites here pass integer-valued x, so the fixed-point string
// is exact and matches JS byte-for-byte; num_to_value then drops any trailing ".0".
fn to_fixed_number(x: f64, digits: usize) -> Value {
    let s = format!("{:.*}", digits, x);
    num_to_value(s.parse::<f64>().unwrap())
}
// JS: n(v) = (v == null || v === '' ? null : Number(v)); Number(number) preserves its representation.
fn n(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) if s.is_empty() => Value::Null,
        Some(x @ Value::Number(_)) => x.clone(),
        Some(other) => num_to_value(js_number_val(other)),
    }
}
// JS: delta(a,b) = (a==null||b==null) ? null : +(a-b).toFixed(a%1||b%1 ? 1 : 0)
fn delta(a: &Value, b: &Value) -> Value {
    if a.is_null() || b.is_null() { return Value::Null; }
    let af = a.as_f64().unwrap();
    let bf = b.as_f64().unwrap();
    let frac = af.fract() != 0.0 || bf.fract() != 0.0;
    to_fixed_number(af - bf, if frac { 1 } else { 0 })
}
fn as_num(v: &Value) -> Option<f64> { if v.is_null() { None } else { v.as_f64() } }
// JS `${number}` for the delta values (integer → "-8", fractional → "0.2").
fn num_str(v: &Value) -> String { match v { Value::Number(x) => x.to_string(), other => js_string(other) } }

// JS: parseJson(s, d) = s ? JSON.parse(s) : d, with a catch → d.
fn parse_json(s: Option<&Value>, d: Value) -> Value {
    if !truthy(s) { return d.clone(); }
    let st = match s { Some(Value::String(x)) => x.clone(), Some(other) => js_string(other), None => return d };
    serde_json::from_str::<Value>(&st).unwrap_or(d)
}

// JS: pathsOf(json) = new Set((parseJson(json,[]) || []).map(x => x && (x.path ?? x)).filter(Boolean))
// Returns the paths in insertion order, de-duplicated (Set semantics).
fn paths_of(json: Option<&Value>) -> Vec<Value> {
    let parsed = parse_json(json, json!([]));
    let arr = if truthy(Some(&parsed)) { parsed } else { json!([]) };
    let items = arr.as_array().cloned().unwrap_or_default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for x in items {
        if !truthy(Some(&x)) { continue; } // x && ... → falsy x is dropped by filter(Boolean)
        let val = match x.get("path").filter(|p| !p.is_null()) { Some(p) => p.clone(), None => x.clone() };
        if !truthy(Some(&val)) { continue; }
        let key = serde_json::to_string(&val).unwrap();
        if seen.insert(key) { out.push(val); }
    }
    out
}

// Howard Hinnant's days-from-civil (proleptic Gregorian, UTC) — matches JS Date.parse of an ISO-Z string.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
// Parse "YYYY-MM-DDTHH:MM:SSZ" (optionally .sss) → epoch ms; anything else → None (JS NaN → null).
fn parse_utc_ms(s: &str) -> Option<i64> {
    if !s.ends_with('Z') { return None; }
    let core = &s[..s.len() - 1];
    let (date, time) = core.split_once('T')?;
    let dp: Vec<&str> = date.split('-').collect();
    if dp.len() != 3 { return None; }
    let y: i64 = dp[0].parse().ok()?;
    let mo: i64 = dp[1].parse().ok()?;
    let da: i64 = dp[2].parse().ok()?;
    let hms = match time.split_once('.') { Some((a, _)) => a, None => time };
    let tp: Vec<&str> = hms.split(':').collect();
    if tp.len() != 3 { return None; }
    let h: i64 = tp[0].parse().ok()?;
    let mi: i64 = tp[1].parse().ok()?;
    let se: i64 = tp[2].parse().ok()?;
    let secs = days_from_civil(y, mo, da) * 86400 + h * 3600 + mi * 60 + se;
    Some(secs * 1000)
}
// JS: ts(at) = Date.parse(String(at).replace(' ','T') + 'Z')  (replace hits the FIRST space only)
fn ts(at: &Value) -> Option<i64> {
    let s = js_string(at);
    let s = match s.find(' ') {
        Some(i) => { let mut o = String::with_capacity(s.len()); o.push_str(&s[..i]); o.push('T'); o.push_str(&s[i + 1..]); o }
        None => s,
    };
    parse_utc_ms(&format!("{s}Z"))
}

// JS `nearest(days)` inside codeHistory — health now vs the point nearest `days` ago.
fn nearest(points: &[Value], now_ms: Option<i64>, now_point: Option<&Value>, days: i64) -> Value {
    let nowms = match now_ms { Some(m) if m != 0 => m, _ => return Value::Null };
    if points.len() < 2 { return Value::Null; }
    let target = nowms - days * 86_400_000;
    let mut best: Option<&Value> = None;
    let mut best_gap = i64::MAX;
    for p in &points[..points.len() - 1] {
        let m = match ts(p.get("at").unwrap_or(&Value::Null)) { Some(x) => x, None => continue };
        let g = (m - target).abs();
        if g < best_gap { best_gap = g; best = Some(p); }
    }
    let best = match best { Some(b) => b, None => return Value::Null };
    let best_health = best.get("health").cloned().unwrap_or(Value::Null);
    let now_health = now_point.and_then(|p| p.get("health")).cloned().unwrap_or(Value::Null);
    if best_health.is_null() || now_health.is_null() { return Value::Null; }
    let d = to_fixed_number(now_health.as_f64().unwrap() - best_health.as_f64().unwrap(), 1);
    json!({ "health": best_health, "at": best.get("at").cloned().unwrap_or(Value::Null), "delta": d })
}

pub fn codeHistory(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('code-history','db') when no db — the db lane always injects one.
    let origin = input.get("origin").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| json!("atlas"));
    let limit_num = match input.get("limit").filter(|v| !v.is_null()) { Some(v) => js_number_val(v), None => 40.0 };
    let limit_i = limit_num.max(2.0).min(200.0) as i64;

    let sql = "SELECT id, created_at, total_files_num, health_score_num, risk_score_num, cycles_num, \
               coverage_pct, smells_total_num, avg_fan_in_num, top_risky_json \
               FROM code_digests WHERE origin = ? ORDER BY id DESC LIMIT ?";
    let mut rows: Vec<Value> = crate::db_json::query_rows(db, sql, &[origin.clone(), json!(limit_i)])
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    rows.reverse(); // chronological (oldest → newest)

    let mut points: Vec<Value> = Vec::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        let files = n(r.get("total_files_num"));
        let health = n(r.get("health_score_num"));
        let risk = n(r.get("risk_score_num"));
        let cycles = n(r.get("cycles_num"));
        let coverage = n(r.get("coverage_pct"));
        let smells = n(r.get("smells_total_num"));
        let fan_in = n(r.get("avg_fan_in_num"));
        let mut p = Map::new();
        p.insert("id".into(), r.get("id").cloned().unwrap_or(Value::Null));
        p.insert("at".into(), r.get("created_at").cloned().unwrap_or(Value::Null));
        p.insert("files".into(), files);
        p.insert("health".into(), health.clone());
        p.insert("risk".into(), risk);
        p.insert("cycles".into(), cycles.clone());
        p.insert("coverage".into(), coverage.clone());
        p.insert("smells".into(), smells);
        p.insert("fan_in".into(), fan_in);
        if i > 0 {
            let q = &rows[i - 1];
            p.insert("d_health".into(), delta(&health, &n(q.get("health_score_num"))));
            p.insert("d_cycles".into(), delta(&cycles, &n(q.get("cycles_num"))));
            p.insert("d_coverage".into(), delta(&coverage, &n(q.get("coverage_pct"))));
        }
        points.push(Value::Object(p));
    }

    // trend — health now vs the rebuild NEAREST a month / quarter ago.
    let now_point = points.last().cloned();
    let now_ms = now_point.as_ref().and_then(|p| ts(p.get("at").unwrap_or(&Value::Null)));
    let trend_now = now_point.as_ref().and_then(|p| p.get("health")).filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
    let trend = json!({
        "now": trend_now,
        "month": nearest(&points, now_ms, now_point.as_ref(), 30),
        "quarter": nearest(&points, now_ms, now_point.as_ref(), 90),
    });

    // incident — the structural diff between the two most recent rebuilds.
    let mut incident = Value::Null;
    if rows.len() >= 2 {
        let cur = &rows[rows.len() - 1];
        let prev = &rows[rows.len() - 2];
        let d_health = delta(&n(cur.get("health_score_num")), &n(prev.get("health_score_num")));
        let d_cycles = delta(&n(cur.get("cycles_num")), &n(prev.get("cycles_num")));
        let d_coverage = delta(&n(cur.get("coverage_pct")), &n(prev.get("coverage_pct")));
        let d_files = delta(&n(cur.get("total_files_num")), &n(prev.get("total_files_num")));
        let d_smells = delta(&n(cur.get("smells_total_num")), &n(prev.get("smells_total_num")));

        // files that ENTERED the riskiest set since last rebuild
        let prev_risky = paths_of(prev.get("top_risky_json"));
        let prev_keys: HashSet<String> = prev_risky.iter().map(|v| serde_json::to_string(v).unwrap()).collect();
        let mut new_risky: Vec<Value> = Vec::new();
        for v in paths_of(cur.get("top_risky_json")) {
            if !prev_keys.contains(&serde_json::to_string(&v).unwrap()) {
                new_risky.push(v);
                if new_risky.len() == 5 { break; }
            }
        }

        // external-surface drift, if the entry-point snapshots recorded a diff this rebuild
        let ep: Value = {
            let fetched = crate::db_json::query_rows(
                db,
                "SELECT diff_json FROM code_entry_point_snapshots WHERE origin=? ORDER BY id DESC LIMIT 1",
                &[origin.clone()],
            );
            match fetched {
                Err(_) => Value::Null, // pre-signals db (table missing) — JS catch → ep stays null
                Ok(rows_ep) => {
                    let eprow = rows_ep.as_array().and_then(|a| a.first());
                    let d = parse_json(eprow.and_then(|r| r.get("diff_json")), Value::Null);
                    if !truthy(Some(&d)) {
                        Value::Null
                    } else {
                        let added = d.get("added").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                        let removed = d.get("removed").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                        if added + removed > 0 { json!({ "added": added as i64, "removed": removed as i64 }) } else { Value::Null }
                    }
                }
            }
        };

        let dh = as_num(&d_health);
        let dc = as_num(&d_cycles);
        let dcov = as_num(&d_coverage);
        let df = as_num(&d_files);
        let ds = as_num(&d_smells);
        let bad = dh.map_or(false, |x| x <= -5.0) || dc.map_or(false, |x| x > 0.0);
        let warn = dh.map_or(false, |x| x < 0.0) || ds.map_or(false, |x| x > 0.0) || dcov.map_or(false, |x| x < 0.0);
        let severity = if bad { "high" } else if warn { "medium" } else if dh.map_or(false, |x| x > 0.0) { "improved" } else { "info" };

        let sign = |v: Option<f64>| if v.map_or(false, |x| x > 0.0) { "+" } else { "" };
        let mut lines: Vec<String> = Vec::new();
        if truthy(Some(&d_health)) { lines.push(format!("health {}{}", sign(dh), num_str(&d_health))); }
        if truthy(Some(&d_cycles)) { lines.push(format!("{}{} import cycle{}", sign(dc), num_str(&d_cycles), if dc.unwrap().abs() == 1.0 { "" } else { "s" })); }
        if truthy(Some(&d_coverage)) { lines.push(format!("coverage {}{}%", sign(dcov), num_str(&d_coverage))); }
        if truthy(Some(&d_smells)) { lines.push(format!("{}{} smell{}", sign(ds), num_str(&d_smells), if ds.unwrap().abs() == 1.0 { "" } else { "s" })); }
        if truthy(Some(&d_files)) { lines.push(format!("{}{} file{}", sign(df), num_str(&d_files), if df.unwrap().abs() == 1.0 { "" } else { "s" })); }
        if truthy(Some(&ep)) {
            let a = ep.get("added").and_then(|v| v.as_i64()).unwrap_or(0);
            let rm = ep.get("removed").and_then(|v| v.as_i64()).unwrap_or(0);
            let p1 = if a != 0 { format!("{a} new") } else { String::new() };
            let p2 = if a != 0 && rm != 0 { " · " } else { "" };
            let p3 = if rm != 0 { format!("{rm} removed") } else { String::new() };
            lines.push(format!("{p1}{p2}{p3} entry point{}", if a + rm == 1 { "" } else { "s" }));
        }

        let summary = if lines.is_empty() {
            "no structural change since the last rebuild".to_string()
        } else {
            format!("since the last rebuild: {}", lines.join(" · "))
        };

        let mut inc = Map::new();
        inc.insert("severity".into(), json!(severity));
        inc.insert("at".into(), cur.get("created_at").cloned().unwrap_or(Value::Null));
        inc.insert("from_at".into(), prev.get("created_at").cloned().unwrap_or(Value::Null));
        inc.insert("summary".into(), json!(summary));
        inc.insert("d_health".into(), d_health);
        inc.insert("d_cycles".into(), d_cycles);
        inc.insert("d_coverage".into(), d_coverage);
        inc.insert("d_files".into(), d_files);
        inc.insert("d_smells".into(), d_smells);
        inc.insert("new_risky".into(), json!(new_risky));
        inc.insert("entry_points".into(), ep);
        incident = Value::Object(inc);
    }

    let mut out = Map::new();
    out.insert("origin".into(), origin);
    out.insert("count".into(), json!(points.len() as i64));
    out.insert("points".into(), json!(points));
    out.insert("trend".into(), trend);
    out.insert("incident".into(), incident);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_digests (id INTEGER PRIMARY KEY, origin TEXT, total_files_num INTEGER, \
             health_score_num INTEGER, risk_score_num INTEGER, cycles_num INTEGER, coverage_pct INTEGER, \
             smells_total_num INTEGER, avg_fan_in_num INTEGER, top_risky_json TEXT, created_at TEXT); \
             CREATE TABLE code_entry_point_snapshots (id INTEGER PRIMARY KEY, origin TEXT, diff_json TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn timeline_deltas_and_incident() {
        let c = setup();
        c.execute(
            "INSERT INTO code_digests (id,origin,total_files_num,health_score_num,risk_score_num,cycles_num,coverage_pct,smells_total_num,avg_fan_in_num,top_risky_json,created_at) \
             VALUES (1,'atlas',100,80,20,1,60,4,2,'[{\"path\":\"a.ts\"}]','2026-06-01 10:00:00')", [],
        ).unwrap();
        c.execute(
            "INSERT INTO code_digests (id,origin,total_files_num,health_score_num,risk_score_num,cycles_num,coverage_pct,smells_total_num,avg_fan_in_num,top_risky_json,created_at) \
             VALUES (2,'atlas',104,72,28,3,57,6,2,'[{\"path\":\"a.ts\"},{\"path\":\"b.ts\"}]','2026-07-01 10:00:00')", [],
        ).unwrap();
        let r = codeHistory(&json!({}), &c).unwrap();
        assert_eq!(r["count"], json!(2));
        assert_eq!(r["points"][1]["health"], json!(72));
        assert_eq!(r["points"][1]["d_health"], json!(-8));
        assert_eq!(r["points"][1]["d_cycles"], json!(2));
        assert_eq!(r["incident"]["severity"], json!("high"));
        assert_eq!(r["incident"]["new_risky"], json!(["b.ts"]));
        assert!(r["incident"]["summary"].as_str().unwrap().contains("health -8"));
        assert!(r["incident"]["summary"].as_str().unwrap().contains("2 import cycles"));
    }

    #[test]
    fn improved_and_empty() {
        let c = setup();
        let empty = codeHistory(&json!({}), &c).unwrap();
        assert_eq!(empty["count"], json!(0));
        assert_eq!(empty["incident"], Value::Null);
        assert_eq!(empty["trend"], json!({ "now": null, "month": null, "quarter": null }));
        c.execute(
            "INSERT INTO code_digests (id,origin,total_files_num,health_score_num,risk_score_num,cycles_num,coverage_pct,smells_total_num,avg_fan_in_num,top_risky_json,created_at) \
             VALUES (1,'atlas',100,60,40,4,50,8,3,'[]','2026-06-01 10:00:00')", [],
        ).unwrap();
        c.execute(
            "INSERT INTO code_digests (id,origin,total_files_num,health_score_num,risk_score_num,cycles_num,coverage_pct,smells_total_num,avg_fan_in_num,top_risky_json,created_at) \
             VALUES (2,'atlas',100,81,19,1,70,3,2,'[]','2026-07-01 10:00:00')", [],
        ).unwrap();
        let r = codeHistory(&json!({}), &c).unwrap();
        assert_eq!(r["incident"]["severity"], json!("improved"));
        assert_eq!(r["incident"]["d_health"], json!(21));
        assert_eq!(r["trend"]["now"], json!(81));
        assert_eq!(r["incident"]["entry_points"], Value::Null);
    }

    #[test]
    fn singular_and_ep_plural() {
        let c = setup();
        c.execute("INSERT INTO code_digests (id,origin,total_files_num,health_score_num,risk_score_num,cycles_num,coverage_pct,smells_total_num,avg_fan_in_num,top_risky_json,created_at) VALUES (1,'atlas',100,80,20,0,60,5,2,'[{\"path\":\"a.ts\"},{\"path\":\"b.ts\"}]','2026-06-01 10:00:00')", []).unwrap();
        c.execute("INSERT INTO code_digests (id,origin,total_files_num,health_score_num,risk_score_num,cycles_num,coverage_pct,smells_total_num,avg_fan_in_num,top_risky_json,created_at) VALUES (2,'atlas',101,79,21,1,60,4,2,'[{\"path\":\"a.ts\"}]','2026-07-01 10:00:00')", []).unwrap();
        c.execute("INSERT INTO code_entry_point_snapshots (id,origin,diff_json) VALUES (1,'atlas','{\"added\":[\"x\"],\"removed\":[]}')", []).unwrap();
        let r = codeHistory(&json!({}), &c).unwrap();
        let s = r["incident"]["summary"].as_str().unwrap();
        assert!(s.contains("+1 import cycle "), "got: {s}");
        assert!(s.contains("-1 smell "), "got: {s}");
        assert!(s.contains("+1 file "), "got: {s}");
        assert!(s.contains("1 new entry point"), "got: {s}");
        assert_eq!(r["incident"]["new_risky"], json!([]));
        assert_eq!(r["incident"]["entry_points"], json!({ "added": 1, "removed": 0 }));
    }
}
