//! cockpit-session-audit.rs — twin of skills/cockpit/cockpit-session-audit.mjs (db READ lane,
//! cross-db ATTACH: brain/timeline). NAMES ARE CONTRACT: `cockpitSessionAudit` (async default →
//! sync twin). SESSION TIMELINE pane: scopes the substrate's three activity ledgers (runtime_events,
//! brain.ai_calls via the session panes' chat_session_ids, timeline.sacred_timeline) to a vibe
//! session. Every query is best-effort — a missing/unattached table never fails the read.
// QUALITY: TBD — 3 independent try-swallow sources joined by chat_session_id + spend/tokens totals
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

// = JS Number(v)
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

// JS `${v}` for a Number: whole-valued finite numbers render without a decimal point.
fn num_tmpl(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 { format!("{}", n as i64) } else { n.to_string() }
}

// node:sqlite binds an integer-valued JS number as INTEGER, else REAL — mirrors that bind type.
fn id_param(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 { json!(n as i64) } else { json!(n) }
}

// JSON output rendering for a JS-computed numeric sum: whole-valued finite numbers render as
// integers (matches JS `${n}` / JSON.stringify — no trailing ".0").
fn js_num_json(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 { json!(n as i64) } else { json!(n) } }

// Mirrors JS `try { db.prepare(sql).all(...params) } catch { /* absent/unattached */ }` — a query
// error (missing table, unattached db) yields zero rows for that source, not a hard failure.
fn safe_query(db: &Connection, sql: &str, params: &[Value]) -> Vec<Value> {
    crate::db_json::query_rows(db, sql, params).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

pub fn cockpitSessionAudit(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let session_id = js_number(input.get("session_id"));
    if session_id == 0.0 || session_id.is_nan() {
        return Err("cockpit-session-audit: session_id is required".into());
    }
    let p = id_param(session_id);

    let session_rows = crate::db_json::query_rows(db, "SELECT id, name, mission_md, status, created_at FROM cockpit_sessions WHERE id = ?", &[p.clone()])?;
    let session = match session_rows.as_array().and_then(|a| a.first()) {
        Some(s) => s.clone(),
        None => {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("error".into(), json!(format!("session {} not found", num_tmpl(session_id))));
            return Ok(Value::Object(out));
        }
    };

    let panes = crate::db_json::query_rows(db, "SELECT id, idx, subagent_name, chat_session_id FROM cockpit_panes WHERE session_id = ?", &[p.clone()])?;
    let panes_arr: Vec<Value> = panes.as_array().cloned().unwrap_or_default();
    let since = str_qq(session.get("created_at"));

    let runtime = safe_query(
        db,
        "SELECT ts, agent, action, skill, duration_ms, outcome, error_code FROM runtime_events \
         WHERE ts >= ? AND action LIKE 'system.cockpit%' ORDER BY id DESC LIMIT 300",
        &[json!(since)],
    );

    let chat_ids: Vec<Value> = panes_arr.iter().filter_map(|pane| pane.get("chat_session_id")).filter(|v| !v.is_null()).cloned().collect();
    let ai_calls: Vec<Value> = if !chat_ids.is_empty() {
        let ph = chat_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        safe_query(
            db,
            &format!(
                "SELECT id, ts, provider, model_id, tier, cost_usd_actual, tokens_in, tokens_out, outcome, subagent \
                 FROM brain.ai_calls WHERE session_id IN ({ph}) ORDER BY id DESC LIMIT 200"
            ),
            &chat_ids,
        )
    } else {
        Vec::new()
    };

    let timeline = safe_query(
        db,
        "SELECT ts, actor, operation, entity_type, entity_id FROM timeline.sacred_timeline \
         WHERE ts >= ? AND (entity_type LIKE '%cockpit%' OR operation LIKE '%cockpit%') \
         ORDER BY id DESC LIMIT 200",
        &[json!(since)],
    );

    // spend/tokens_out: `Number(a.field) || 0` — a NaN or 0 contribution is indistinguishable from 0.
    let spend: f64 = ai_calls.iter().map(|a| { let n = js_number(a.get("cost_usd_actual")); if n.is_nan() { 0.0 } else { n } }).sum();
    let tokens_out: f64 = ai_calls.iter().map(|a| { let n = js_number(a.get("tokens_out")); if n.is_nan() { 0.0 } else { n } }).sum();

    let mut totals = Map::new();
    totals.insert("runtime".into(), json!(runtime.len() as i64));
    totals.insert("ai_calls".into(), json!(ai_calls.len() as i64));
    totals.insert("timeline".into(), json!(timeline.len() as i64));
    totals.insert("spend".into(), js_num_json(spend));
    totals.insert("tokens_out".into(), js_num_json(tokens_out));

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("session".into(), session);
    out.insert("panes".into(), json!(panes_arr.len() as i64));
    out.insert("runtime".into(), json!(runtime));
    out.insert("ai_calls".into(), json!(ai_calls));
    out.insert("timeline".into(), json!(timeline));
    out.insert("totals".into(), Value::Object(totals));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_schema_only() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c
    }

    fn full_db() -> Connection {
        let c = seeded_schema_only();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS timeline; \
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, agent TEXT, action TEXT, skill TEXT, duration_ms INTEGER, outcome TEXT, error_code TEXT); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, provider TEXT, model_id TEXT, tier TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, outcome TEXT, subagent TEXT, session_id INTEGER); \
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT); \
             INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (1,'alpha','active','2026-01-01 00:00:10'); \
             INSERT INTO cockpit_panes (id,session_id,idx,subagent_name,chat_session_id) VALUES (10,1,0,'ui',501),(11,1,1,'lib',NULL); \
             INSERT INTO runtime_events (id,ts,agent,action,skill,duration_ms,outcome,error_code) VALUES \
               (1,'2026-01-01 00:00:20','dev','system.cockpit.pane.update','cockpit-pane-update',12,'ok',NULL), \
               (2,'2026-01-01 00:00:05','dev','system.cockpit.pane.update','cockpit-pane-update',9,'ok',NULL), \
               (3,'2026-01-01 00:00:30','dev','system.card.add','card-add',5,'ok',NULL), \
               (4,'2026-01-01 00:00:25','dev','system.cockpit.overview','cockpit-overview',3,'ok',NULL); \
             INSERT INTO brain.ai_calls (id,ts,provider,model_id,tier,cost_usd_actual,tokens_in,tokens_out,outcome,subagent,session_id) VALUES \
               (100,'2026-01-01 00:00:40','anthropic','claude-x','mid',0.5,10,20,'success','pane:10',501), \
               (101,'2026-01-01 00:00:41','anthropic','claude-y','low',NULL,5,NULL,'success','pane:10',501), \
               (102,'2026-01-01 00:00:42','anthropic','claude-z','low',0.25,1,5,'success','other',999); \
             INSERT INTO timeline.sacred_timeline (id,ts,actor,operation,entity_type,entity_id) VALUES \
               (1,'2026-01-01 00:00:15','dev','cockpit.pane.update','cockpit_pane','10'), \
               (2,'2026-01-01 00:00:02','dev','cockpit.pane.update','cockpit_pane','10'), \
               (3,'2026-01-01 00:00:16','dev','card.add','card','C-1'), \
               (4,'2026-01-01 00:00:18','dev','cockpit.session.audit','card','C-2');",
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_session_id_errors() {
        let c = seeded_schema_only();
        assert!(cockpitSessionAudit(&json!({}), &c).unwrap_err().contains("session_id is required"));
    }

    #[test]
    fn unknown_session_not_found() {
        let c = seeded_schema_only();
        let r = cockpitSessionAudit(&json!({ "session_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "session 999 not found" }));
    }

    #[test]
    fn absent_tables_best_effort_empty() {
        let c = seeded_schema_only();
        c.execute_batch("INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (1,'alpha','active','2026-01-01 00:00:01')").unwrap();
        let r = cockpitSessionAudit(&json!({ "session_id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["runtime"], json!([]));
        assert_eq!(r["ai_calls"], json!([]));
        assert_eq!(r["timeline"], json!([]));
        assert_eq!(r["panes"], json!(0));
        assert_eq!(r["totals"], json!({ "runtime": 0, "ai_calls": 0, "timeline": 0, "spend": 0, "tokens_out": 0 }));
    }

    #[test]
    fn scopes_and_sums_totals() {
        let c = full_db();
        let r = cockpitSessionAudit(&json!({ "session_id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["ok", "session", "panes", "runtime", "ai_calls", "timeline", "totals"]);
        assert_eq!(r["session"]["name"], json!("alpha"));
        assert_eq!(r["panes"], json!(2));

        let runtime = r["runtime"].as_array().unwrap();
        assert_eq!(runtime.len(), 2);
        let actions: Vec<&str> = runtime.iter().map(|x| x["action"].as_str().unwrap()).collect();
        assert_eq!(actions, vec!["system.cockpit.overview", "system.cockpit.pane.update"]); // id DESC

        let ai_calls = r["ai_calls"].as_array().unwrap();
        let ids: Vec<i64> = ai_calls.iter().map(|x| x["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![101, 100]); // id DESC, session_id 999 excluded

        let timeline = r["timeline"].as_array().unwrap();
        let entity_ids: Vec<&str> = timeline.iter().map(|x| x["entity_id"].as_str().unwrap()).collect();
        assert_eq!(entity_ids, vec!["C-2", "10"]); // id DESC

        assert_eq!(r["totals"], json!({ "runtime": 2, "ai_calls": 2, "timeline": 2, "spend": 0.5, "tokens_out": 20 }));
    }
}
