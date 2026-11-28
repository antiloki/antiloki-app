//! substrate-telemetry.rs — twin of skills/codebase/substrate-telemetry.mjs (db READ, cross-db).
//! NAMES ARE CONTRACT: `substrateTelemetry` (default). "Grafana for antiloki itself": AI cost /
//! latency / tokens from brain.ai_calls + the rebuild's own phase timings (code_onboard_jobs).
//!
//! Frozen clock: the JS builds `datetime('now', '-${hours} hours')`; the conform JS lane pins
//! 'now' to 2026-01-01 00:00:00, so this twin writes the frozen literal with the same dynamic
//! modifier. The JS `if (!db) throw err.contextMissing(...)` guard is unreachable in the db lane
//! (a connection is always injected), so it is not transcribed.
//!
//! Rounding: JS `round = (n,d) => (n == null ? null : +Number(n).toFixed(d))`. Rust `{:.d}`
//! formatting rounds exact decimal ties half-to-even where JS picks the larger n — fixtures use
//! tie-free aggregates. `Math.round` (rebuild) IS transcribed tie-exact (half toward +inf).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` left-operand test
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS Math.round: nearest, exact .5 ties toward +infinity (NOT away from zero, NOT to even).
fn js_round(x: f64) -> f64 { let f = x.floor(); if x - f >= 0.5 { f + 1.0 } else { f } }
// JS `+Number(n).toFixed(d)` for a finite n, collapsed to integer when whole (JSON.stringify(5.0) is "5").
fn to_fixed_num(x: f64, d: usize) -> Value {
    let p: f64 = format!("{:.*}", d, x).parse().unwrap();
    if p.fract() == 0.0 && p.abs() < 9.007_199_254_740_992e15 { json!(p as i64) } else { json!(p) }
}
// = the module's `round(n, d)`: null in → null out, else +toFixed.
fn round_opt(v: Option<&Value>, d: usize) -> Value {
    match v { None | Some(Value::Null) => Value::Null, Some(x) => to_fixed_num(x.as_f64().unwrap_or(f64::NAN), d) }
}
// JS `Number(ms) || 0` — NaN and ±0 collapse to 0.
fn num_or_zero(v: &Value) -> f64 { let n = js_number(Some(v)); if n.is_nan() || n == 0.0 { 0.0 } else { n } }

pub fn substrateTelemetry(input: &Value, db: &Connection) -> Result<Value, String> {
    // hours = Math.min(Math.max(Math.round(Number(input?.window_hours ?? 720)), 1), 24*365)
    let n = match nz(input.get("window_hours")) { None => 720.0, Some(v) => js_number(Some(v)) };
    let r = js_round(n);
    // JS Math.max/min propagate NaN (window_hours: null in JSON output, 'NaN' in the SQL modifier);
    // Rust f64::max would swallow it, so branch explicitly.
    let (hours_str, window_hours_v) = if r.is_nan() {
        ("NaN".to_string(), Value::Null)
    } else {
        let h = r.max(1.0).min(8760.0);
        (format!("{}", h as i64), json!(h as i64))
    };
    // JS: datetime('now', '-${hours} hours') — crate::clock picks wall-clock vs the pinned instant.
    let since = format!("datetime({}, '-{} hours')", crate::clock::now_sql_inner(), hours_str);

    // ── the substrate's own AI runtime (brain.ai_calls) ──
    let mut ai = Map::new();
    ai.insert("calls".into(), json!(0));
    ai.insert("cost_usd".into(), json!(0));
    ai.insert("tokens_in".into(), json!(0));
    ai.insert("tokens_out".into(), json!(0));
    ai.insert("cache_hit_pct".into(), Value::Null);
    ai.insert("avg_ms".into(), Value::Null);
    ai.insert("series".into(), json!([]));
    ai.insert("by_prompt".into(), json!([]));
    ai.insert("by_provider".into(), json!([]));
    ai.insert("by_tier".into(), json!([]));
    // JS try{...}catch{}: one try wraps the six statements sequentially — a mid-block failure
    // keeps the assignments already made. The `?` early-returns of this closure mirror that.
    let _ = (|| -> Result<(), String> {
        let t_rows = crate::db_json::query_rows(db, &format!("SELECT count(*) calls, COALESCE(sum(cost_usd_actual),0) cost, COALESCE(sum(tokens_in),0) ti,
      COALESCE(sum(tokens_out),0) to2, AVG(duration_ms) avg_ms, AVG(CASE WHEN cached=1 THEN 1.0 ELSE 0.0 END) cache
      FROM brain.ai_calls WHERE ts >= {since}"), &[])?;
        let t = t_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        ai.insert("calls".into(), t.get("calls").cloned().unwrap_or(Value::Null));
        ai.insert("cost_usd".into(), round_opt(t.get("cost"), 4));
        ai.insert("tokens_in".into(), t.get("ti").cloned().unwrap_or(Value::Null));
        ai.insert("tokens_out".into(), t.get("to2").cloned().unwrap_or(Value::Null));
        ai.insert("avg_ms".into(), round_opt(t.get("avg_ms"), 0));
        ai.insert("cache_hit_pct".into(), if truthy(t.get("calls")) { to_fixed_num(js_number(t.get("cache")) * 100.0, 1) } else { Value::Null });

        let series = crate::db_json::query_rows(db, &format!("SELECT date(ts) day, count(*) calls, COALESCE(sum(cost_usd_actual),0) cost, AVG(duration_ms) avg_ms
      FROM brain.ai_calls WHERE ts >= {since} GROUP BY date(ts) ORDER BY day"), &[])?;
        ai.insert("series".into(), json!(series.as_array().unwrap().iter().map(|r| {
            let mut o = Map::new();
            o.insert("day".into(), r.get("day").cloned().unwrap_or(Value::Null));
            o.insert("calls".into(), r.get("calls").cloned().unwrap_or(Value::Null));
            o.insert("cost".into(), round_opt(r.get("cost"), 4));
            o.insert("avg_ms".into(), round_opt(r.get("avg_ms"), 0));
            Value::Object(o)
        }).collect::<Vec<_>>()));

        let by_prompt = crate::db_json::query_rows(db, &format!("SELECT p.name name, count(*) calls, COALESCE(sum(a.cost_usd_actual),0) cost, AVG(a.duration_ms) avg_ms
      FROM brain.ai_calls a LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id
      WHERE a.ts >= {since} GROUP BY p.name ORDER BY cost DESC, calls DESC LIMIT 12"), &[])?;
        ai.insert("by_prompt".into(), json!(by_prompt.as_array().unwrap().iter().map(|r| {
            let mut o = Map::new();
            o.insert("name".into(), nz(r.get("name")).cloned().unwrap_or(json!("(unknown)"))); // r.name ?? '(unknown)'
            o.insert("calls".into(), r.get("calls").cloned().unwrap_or(Value::Null));
            o.insert("cost".into(), round_opt(r.get("cost"), 4));
            o.insert("avg_ms".into(), round_opt(r.get("avg_ms"), 0));
            Value::Object(o)
        }).collect::<Vec<_>>()));

        let by_provider = crate::db_json::query_rows(db, &format!("SELECT COALESCE(provider,'?') provider, count(*) calls, COALESCE(sum(cost_usd_actual),0) cost
      FROM brain.ai_calls WHERE ts >= {since} GROUP BY provider ORDER BY cost DESC"), &[])?;
        ai.insert("by_provider".into(), json!(by_provider.as_array().unwrap().iter().map(|r| {
            let mut o = Map::new();
            o.insert("provider".into(), r.get("provider").cloned().unwrap_or(Value::Null));
            o.insert("calls".into(), r.get("calls").cloned().unwrap_or(Value::Null));
            o.insert("cost".into(), round_opt(r.get("cost"), 4));
            Value::Object(o)
        }).collect::<Vec<_>>()));

        let by_tier = crate::db_json::query_rows(db, &format!("SELECT tier, count(*) calls, COALESCE(sum(cost_usd_actual),0) cost
      FROM brain.ai_calls WHERE ts >= {since} GROUP BY tier ORDER BY calls DESC"), &[])?;
        ai.insert("by_tier".into(), json!(by_tier.as_array().unwrap().iter().map(|r| {
            let mut o = Map::new();
            o.insert("tier".into(), r.get("tier").cloned().unwrap_or(Value::Null));
            o.insert("calls".into(), r.get("calls").cloned().unwrap_or(Value::Null));
            o.insert("cost".into(), round_opt(r.get("cost"), 4));
            Value::Object(o)
        }).collect::<Vec<_>>()));
        Ok(())
    })();

    // ── the tool's own build performance (rebuild phase_ms) ──
    let mut rebuild = Map::new();
    rebuild.insert("runs".into(), json!(0));
    rebuild.insert("phases".into(), json!([]));
    rebuild.insert("series".into(), json!([]));
    // JS try/catch: table absent → query throws → rebuild keeps its zero state.
    if let Ok(rows_v) = crate::db_json::query_rows(db, "SELECT updated_at, detail FROM code_onboard_jobs WHERE status='done' AND detail IS NOT NULL ORDER BY rowid DESC LIMIT 30", &[]) {
        let mut rows = rows_v.as_array().cloned().unwrap_or_default();
        rows.reverse(); // DESC fetch reversed → oldest-first walk
        let mut agg: Vec<(String, f64, i64, f64)> = Vec::new(); // phase → (sum, n, last), JS Map insertion order
        let mut series: Vec<Value> = Vec::new();
        for r in &rows {
            // parseJson(r.detail, null)?.phase_ms — parse failure / empty string → null → skip
            let parsed: Value = r.get("detail").and_then(|d| d.as_str()).filter(|s| !s.is_empty())
                .and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
            let pm = parsed.get("phase_ms"); // ?. on non-objects → undefined (None)
            // if (!pm || typeof pm !== 'object') continue — objects AND arrays pass (Object.entries of an array yields index keys)
            let entries: Vec<(String, f64)> = match pm {
                Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), num_or_zero(v))).collect(),
                Some(Value::Array(a)) => a.iter().enumerate().map(|(i, v)| (i.to_string(), num_or_zero(v))).collect(),
                _ => continue,
            };
            let mut total = 0.0f64;
            for (ph, m) in entries {
                total += m;
                match agg.iter().position(|e| e.0 == ph) {
                    Some(i) => { let e = &mut agg[i]; e.1 += m; e.2 += 1; e.3 = m; }
                    None => agg.push((ph, m, 1, m)),
                }
            }
            let mut o = Map::new();
            o.insert("at".into(), r.get("updated_at").cloned().unwrap_or(Value::Null));
            o.insert("total_ms".into(), json!(js_round(total) as i64));
            series.push(Value::Object(o));
        }
        rebuild.insert("runs".into(), json!(series.len()));
        rebuild.insert("series".into(), json!(series));
        let mut phases: Vec<Value> = agg.iter().map(|(ph, sum, n, last)| {
            let mut p = Map::new();
            p.insert("phase".into(), json!(ph));
            p.insert("avg_ms".into(), json!(js_round(sum / *n as f64) as i64));
            p.insert("last_ms".into(), json!(js_round(*last) as i64));
            Value::Object(p)
        }).collect();
        phases.sort_by(|a, b| b["avg_ms"].as_i64().unwrap_or(0).cmp(&a["avg_ms"].as_i64().unwrap_or(0))); // stable desc, ties keep insertion order
        rebuild.insert("phases".into(), json!(phases));
    }

    let mut out = Map::new();
    out.insert("window_hours".into(), window_hours_v);
    out.insert("ai".into(), Value::Object(ai));
    out.insert("rebuild".into(), Value::Object(rebuild));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;\n             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, provider TEXT, tier TEXT, duration_ms INTEGER, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, cached INTEGER, ts TEXT);\n             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT);\n             CREATE TABLE code_onboard_jobs (job_id TEXT, status TEXT, detail TEXT, updated_at TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn aggregates_ai_and_rebuild_with_exact_shape() {
        let c = full_db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id, name) VALUES (1, 'atlas.story.propose');\n             INSERT INTO brain.ai_calls (id, prompt_template_id, provider, tier, duration_ms, cost_usd_actual, tokens_in, tokens_out, cached, ts) VALUES\n               (1, 1, 'anthropic', 'mid', 400, 0.002, 500, 200, 0, '2025-12-30 09:00:00'),\n               (2, 1, 'anthropic', 'mid', 600, 0.004, 800, 300, 0, '2025-12-30 15:00:00'),\n               (3, NULL, 'openrouter', 'high', 100, 0.0, 0, 0, 1, '2025-12-31 10:00:00'),\n               (4, 1, 'anthropic', 'mid', 999, 9.9, 9, 9, 0, '2025-11-01 08:00:00');\n             INSERT INTO code_onboard_jobs (job_id, status, detail, updated_at) VALUES\n               ('j1', 'done', '{\"phase_ms\":{\"ingest\":10000,\"metrics\":4000}}', '2025-12-30 10:00:00'),\n               ('j2', 'done', 'not json', '2025-12-30 11:00:00'),\n               ('j3', 'done', '{\"phase_ms\":{\"ingest\":20000,\"metrics\":3000,\"compile\":30000}}', '2025-12-31 10:00:00'),\n               ('j4', 'running', '{\"phase_ms\":{\"ingest\":1}}', '2025-12-31 11:00:00'),\n               ('j5', 'done', NULL, '2025-12-31 12:00:00');",
        ).unwrap();
        let r = substrateTelemetry(&json!({}), &c).unwrap();
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"window_hours":720,"ai":{"calls":3,"cost_usd":0.006,"tokens_in":1300,"tokens_out":500,"cache_hit_pct":33.3,"avg_ms":367,"series":[{"day":"2025-12-30","calls":2,"cost":0.006,"avg_ms":500},{"day":"2025-12-31","calls":1,"cost":0,"avg_ms":100}],"by_prompt":[{"name":"atlas.story.propose","calls":2,"cost":0.006,"avg_ms":500},{"name":"(unknown)","calls":1,"cost":0,"avg_ms":100}],"by_provider":[{"provider":"anthropic","calls":2,"cost":0.006},{"provider":"openrouter","calls":1,"cost":0}],"by_tier":[{"tier":"mid","calls":2,"cost":0.006},{"tier":"high","calls":1,"cost":0}]},"rebuild":{"runs":2,"phases":[{"phase":"compile","avg_ms":30000,"last_ms":30000},{"phase":"ingest","avg_ms":15000,"last_ms":20000},{"phase":"metrics","avg_ms":3500,"last_ms":3000}],"series":[{"at":"2025-12-30 10:00:00","total_ms":14000},{"at":"2025-12-31 10:00:00","total_ms":53000}]}}"#
        );
    }

    #[test]
    fn window_clamps_and_string_coercion() {
        let c = full_db();
        // in-window only for a 24h window; the older row is out
        c.execute_batch(
            "INSERT INTO brain.ai_calls (id, provider, tier, duration_ms, cost_usd_actual, tokens_in, tokens_out, cached, ts) VALUES\n               (1, 'anthropic', 'mid', 100, 0.001, 10, 5, 0, '2025-12-31 12:00:00'),\n               (2, 'anthropic', 'mid', 900, 0.009, 90, 45, 0, '2025-12-28 00:00:00');",
        ).unwrap();
        let r = substrateTelemetry(&json!({ "window_hours": "24" }), &c).unwrap();
        assert_eq!(r["window_hours"], json!(24));
        assert_eq!(r["ai"]["calls"], json!(1));
        assert_eq!(r["ai"]["cost_usd"], json!(0.001));
        // clamps: 0 → 1, huge → 8760; Math.round half toward +inf: 30.5 → 31
        assert_eq!(substrateTelemetry(&json!({ "window_hours": 0 }), &c).unwrap()["window_hours"], json!(1));
        assert_eq!(substrateTelemetry(&json!({ "window_hours": 100000.7 }), &c).unwrap()["window_hours"], json!(8760));
        assert_eq!(substrateTelemetry(&json!({ "window_hours": 30.5 }), &c).unwrap()["window_hours"], json!(31));
        // non-numeric → NaN window: window_hours null, no rows match
        let nan = substrateTelemetry(&json!({ "window_hours": "abc" }), &c).unwrap();
        assert_eq!(nan["window_hours"], Value::Null);
        assert_eq!(nan["ai"]["calls"], json!(0));
    }

    #[test]
    fn zero_state_and_tie_rounding_in_phases() {
        let bare = Connection::open_in_memory().unwrap(); // no brain, no jobs table → both catch paths
        let z = substrateTelemetry(&json!({}), &bare).unwrap();
        assert_eq!(
            serde_json::to_string(&z).unwrap(),
            r#"{"window_hours":720,"ai":{"calls":0,"cost_usd":0,"tokens_in":0,"tokens_out":0,"cache_hit_pct":null,"avg_ms":null,"series":[],"by_prompt":[],"by_provider":[],"by_tier":[]},"rebuild":{"runs":0,"phases":[],"series":[]}}"#
        );

        let c = full_db();
        // avg 100.5 must round to 101 (JS half toward +inf), empty phase_ms {} still counts a run,
        // non-numeric ms → 0, null updated_at flows through as null
        c.execute_batch(
            "INSERT INTO code_onboard_jobs (job_id, status, detail, updated_at) VALUES\n               ('t1', 'done', '{\"phase_ms\":{\"tie\":100,\"junk\":\"abc\"}}', '2025-12-30 10:00:00'),\n               ('t2', 'done', '{\"phase_ms\":{\"tie\":101}}', NULL),\n               ('t3', 'done', '{\"phase_ms\":{}}', '2025-12-31 09:00:00'),\n               ('t4', 'done', '{\"other\":1}', '2025-12-31 10:00:00');",
        ).unwrap();
        let r = substrateTelemetry(&json!({}), &c).unwrap();
        assert_eq!(r["rebuild"]["runs"], json!(3)); // t4 skipped (no phase_ms), t3 empty object still pushed
        assert_eq!(r["rebuild"]["series"][1]["at"], Value::Null);
        assert_eq!(r["rebuild"]["series"][2]["total_ms"], json!(0));
        let phases = r["rebuild"]["phases"].as_array().unwrap();
        assert_eq!(phases[0], json!({ "phase": "tie", "avg_ms": 101, "last_ms": 101 })); // 201/2 = 100.5 → 101
        assert_eq!(phases[1], json!({ "phase": "junk", "avg_ms": 0, "last_ms": 0 })); // Number('abc') || 0
    }
}
