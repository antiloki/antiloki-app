//! bypasses-list.rs — twin of skills/system/bypasses-list.mjs (db READ lane, cross-db laws via
//! ATTACH). NAMES ARE CONTRACT: `bypassesList` (default). Reads law_violations (joined with
//! laws for slug/kind) plus external-traffic-interceptor notifications, classifies each row's
//! bypass kind from its context/payload JSON, merges + re-sorts by ts desc, caps to limit.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// Standard helpers (playbook, copied verbatim).
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }

pub fn bypassesList(input: &Value, db: &Connection) -> Result<Value, String> {
    let since_ts_raw = input.get("since_ts");
    let since_ts_str: Option<String> = if truthy(since_ts_raw) { Some(js_string(since_ts_raw.unwrap())) } else { None };

    // cappedLimit = Math.min(Math.max(1, Number(limit) || 100), 500)
    // (destructure default `limit = 100` fires only for a MISSING key; an explicit null still
    // flows through Number(null)=0 -> ||100, so both paths land on the same final 100.)
    let limit_raw: f64 = match input.get("limit") { None => 100.0, Some(v) => js_number(Some(v)) };
    let limit_or = if limit_raw == 0.0 || limit_raw.is_nan() { 100.0 } else { limit_raw };
    let capped_limit_f = limit_or.max(1.0).min(500.0);
    let capped_limit: i64 = capped_limit_f.trunc() as i64; // JS ToIntegerOrInfinity truncation (both LIMIT + final .slice use this)

    let mut sql1 = String::from(
        "SELECT lv.id, lv.attempted_at AS ts, lv.attempted_by AS actor, lv.agent_name, lv.action_name, lv.outcome, lv.context_json, l.slug AS law_slug, l.kind AS law_kind FROM law_violations lv LEFT JOIN laws.laws l ON l.id = lv.law_id"
    );
    let mut params1: Vec<Value> = Vec::new();
    if let Some(s) = &since_ts_str {
        sql1.push_str(" WHERE attempted_at >= ?");
        params1.push(json!(s));
    }
    sql1.push_str(" ORDER BY lv.id DESC LIMIT ?");
    params1.push(json!(capped_limit));

    let rows1 = match crate::db_json::query_rows(db, &sql1, &params1) {
        Ok(r) => r,
        Err(_) => return Ok(json!([])),
    };

    let mut from_law_violations: Vec<Value> = Vec::new();
    for r in rows1.as_array().unwrap() {
        let context_json = r.get("context_json");
        let ctx: Option<Value> = if truthy(context_json) {
            context_json.and_then(|v| v.as_str()).and_then(|s| serde_json::from_str::<Value>(s).ok())
        } else {
            None
        };
        let ctx_field = |k: &str| -> Option<Value> { ctx.as_ref().and_then(|c| c.get(k)).cloned() };

        let mut kind = "law_blocked";
        if ctx_field("god") == Some(json!(true)) {
            kind = "god_dispatch";
        } else if truthy(ctx_field("unsafe_skill").as_ref()) {
            kind = "unsafe_skill";
        } else if truthy(ctx_field("no_verify").as_ref()) {
            kind = "no_verify";
        }

        let actor = nz(r.get("actor")).or_else(|| nz(r.get("agent_name"))).cloned().unwrap_or(json!("unknown"));
        let reason_a = ctx_field("reason");
        let reason_b = ctx_field("suggested_path");
        let reason = nz(reason_a.as_ref()).or_else(|| nz(reason_b.as_ref())).cloned().unwrap_or(Value::Null);
        let outcome = nz(r.get("outcome")).cloned().unwrap_or(json!("blocked"));

        let mut o = Map::new();
        o.insert("id".into(), r.get("id").cloned().unwrap_or(Value::Null));
        o.insert("ts".into(), r.get("ts").cloned().unwrap_or(Value::Null));
        o.insert("kind".into(), json!(kind));
        o.insert("actor".into(), actor);
        o.insert("action_name".into(), r.get("action_name").cloned().unwrap_or(Value::Null));
        o.insert("law_slug".into(), r.get("law_slug").cloned().unwrap_or(Value::Null));
        o.insert("reason".into(), reason);
        o.insert("outcome".into(), outcome);
        from_law_violations.push(Value::Object(o));
    }

    // FEAT-BE-AI-DETECTION-AUDIT-WARN: external-traffic notifications surfaced as bypasses too.
    let mut sql2 = String::from(
        "SELECT id, ts, kind, source, message, payload_json FROM notifications WHERE source = 'external-traffic-interceptor'"
    );
    let mut params2: Vec<Value> = Vec::new();
    if let Some(s) = &since_ts_str {
        sql2.push_str(" AND ts >= ?");
        params2.push(json!(s));
    }
    sql2.push_str(" ORDER BY id DESC LIMIT ?");
    params2.push(json!(capped_limit));

    let mut from_notifs: Vec<Value> = Vec::new();
    if let Ok(rows2) = crate::db_json::query_rows(db, &sql2, &params2) {
        for n in rows2.as_array().unwrap() {
            let payload_json = n.get("payload_json");
            let payload: Option<Value> = if truthy(payload_json) {
                payload_json.and_then(|v| v.as_str()).and_then(|s| serde_json::from_str::<Value>(s).ok())
            } else {
                None
            };
            let payload_field = |k: &str| -> Option<Value> { payload.as_ref().and_then(|p| p.get(k)).cloned() };

            let kind = nz(payload_field("kind_detail").as_ref()).cloned().unwrap_or(json!("external_http_dispatch"));
            let actor = nz(payload_field("source").as_ref()).cloned().unwrap_or(json!("unknown"));
            let action_name = nz(payload_field("path").as_ref()).cloned().unwrap_or(Value::Null);
            let reason = nz(payload_field("user_agent").as_ref()).cloned().unwrap_or(Value::Null);

            let mut o = Map::new();
            o.insert("id".into(), json!(format!("notif-{}", tmpl(n.get("id")))));
            o.insert("ts".into(), n.get("ts").cloned().unwrap_or(Value::Null));
            o.insert("kind".into(), kind);
            o.insert("actor".into(), actor);
            o.insert("action_name".into(), action_name);
            o.insert("law_slug".into(), Value::Null);
            o.insert("reason".into(), reason);
            o.insert("outcome".into(), json!("logged"));
            from_notifs.push(Value::Object(o));
        }
    }

    let mut merged: Vec<Value> = Vec::new();
    merged.extend(from_law_violations);
    merged.extend(from_notifs);
    // String(b.ts).localeCompare(String(a.ts)) desc; sort_by is stable like JS's sort.
    merged.sort_by(|a, b| {
        let bs = js_string(b.get("ts").unwrap_or(&Value::Null));
        let as_ = js_string(a.get("ts").unwrap_or(&Value::Null));
        bs.cmp(&as_)
    });
    merged.truncate(capped_limit as usize);
    Ok(json!(merged))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, severity TEXT, source TEXT, message TEXT, related_card_slug TEXT, payload_json TEXT, dismissed_at TEXT); \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT); \
          CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER, attempted_at TEXT, attempted_by TEXT, agent_name TEXT, action_name TEXT, context_json TEXT, outcome TEXT); \
          INSERT INTO laws.laws (id,slug,kind,domain) VALUES \
            (1,'NEVER_BYPASS_WITH_AUDIT','constitutional','audit'), \
            (2,'ASK_BEFORE_DESTRUCTIVE','procedural','security'); \
          INSERT INTO laws.law_violations (id,law_id,attempted_at,attempted_by,agent_name,action_name,context_json,outcome) VALUES \
            (1,1,'2025-12-28 09:00:00','agent:dev','dev','dev.file.write','{\"god\":true,\"reason\":\"testing god bypass\"}','blocked'), \
            (2,1,'2025-12-29 10:00:00',NULL,'qa','qa.card.update','{\"unsafe_skill\":\"cli-flag\"}','warned'), \
            (3,2,'2025-12-30 11:00:00','agent:dev','dev','dev.card.delete','{\"no_verify\":1,\"suggested_path\":\"use dev.card.archive instead\"}',NULL), \
            (4,2,'2025-12-31 12:00:00','human:dani',NULL,NULL,NULL,'allowed_exception'), \
            (5,99,'2026-01-01 08:00:00',NULL,NULL,'ghost.action','not-json{','escaped'), \
            (6,1,'2025-12-27 06:00:00','agent:sneaky','sneaky','sneaky.action','{\"god\":1,\"reason\":\"not real god\"}','blocked'); \
          INSERT INTO notifications (id,ts,kind,severity,source,message,related_card_slug,payload_json,dismissed_at) VALUES \
            (1,'2025-12-30 12:00:00','generic','warn','external-traffic-interceptor','hit from unknown IP',NULL,'{\"kind_detail\":\"ai_process_probe\",\"source\":\"203.0.113.5\",\"path\":\"/api/cards\",\"user_agent\":\"curl/8.0\"}',NULL), \
            (2,'2025-12-31 09:30:00','generic','info','external-traffic-interceptor','no payload',NULL,NULL,NULL), \
            (3,'2026-01-01 07:00:00','generic','warn','external-traffic-interceptor','bad payload',NULL,'{not valid json',NULL), \
            (4,'2025-12-29 05:00:00','generic','info','other-source','should be excluded',NULL,NULL,NULL);").unwrap();
        c
    }

    #[test]
    fn classifies_merges_sorts() {
        let c = db();
        let all = bypassesList(&json!({}), &c).unwrap();
        let rows = all.as_array().unwrap();
        // 6 law_violations + 3 matching notifications (other-source excluded), sorted ts desc
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[0]["id"], json!(5)); // 2026-01-01 08:00:00, unmatched law_id -> law_slug null
        assert_eq!(rows[0]["law_slug"], Value::Null);
        assert_eq!(rows[0]["kind"], json!("law_blocked")); // invalid JSON context -> ctx=null -> default
        assert_eq!(rows[0]["actor"], json!("unknown")); // both attempted_by + agent_name null

        // god:true strict-equality branch + reason from ctx.reason
        let god_row = rows.iter().find(|r| r["id"] == json!(1)).unwrap();
        assert_eq!(god_row["kind"], json!("god_dispatch"));
        assert_eq!(god_row["reason"], json!("testing god bypass"));
        assert_eq!(god_row["law_slug"], json!("NEVER_BYPASS_WITH_AUDIT"));

        // god:1 (truthy but not === true) must NOT trigger god_dispatch
        let sneaky_row = rows.iter().find(|r| r["id"] == json!(6)).unwrap();
        assert_eq!(sneaky_row["kind"], json!("law_blocked"));
        assert_eq!(sneaky_row["reason"], json!("not real god"));

        // unsafe_skill truthy (non-boolean string) + actor falls back to agent_name
        let unsafe_row = rows.iter().find(|r| r["id"] == json!(2)).unwrap();
        assert_eq!(unsafe_row["kind"], json!("unsafe_skill"));
        assert_eq!(unsafe_row["actor"], json!("qa"));
        assert_eq!(unsafe_row["reason"], Value::Null);

        // no_verify truthy numeric + reason falls back to suggested_path + outcome null -> 'blocked'
        let noverify_row = rows.iter().find(|r| r["id"] == json!(3)).unwrap();
        assert_eq!(noverify_row["kind"], json!("no_verify"));
        assert_eq!(noverify_row["reason"], json!("use dev.card.archive instead"));
        assert_eq!(noverify_row["outcome"], json!("blocked"));

        // notif rows: id prefix, kind_detail/source/path/user_agent used when present
        let notif1 = rows.iter().find(|r| r["id"] == json!("notif-1")).unwrap();
        assert_eq!(notif1["kind"], json!("ai_process_probe"));
        assert_eq!(notif1["actor"], json!("203.0.113.5"));
        assert_eq!(notif1["action_name"], json!("/api/cards"));
        assert_eq!(notif1["reason"], json!("curl/8.0"));
        assert_eq!(notif1["outcome"], json!("logged"));
        assert_eq!(notif1["law_slug"], Value::Null);

        // notif with NULL payload_json -> defaults
        let notif2 = rows.iter().find(|r| r["id"] == json!("notif-2")).unwrap();
        assert_eq!(notif2["kind"], json!("external_http_dispatch"));
        assert_eq!(notif2["actor"], json!("unknown"));
        assert_eq!(notif2["action_name"], Value::Null);

        // notif with invalid JSON payload -> parse fails -> defaults, same as NULL
        let notif3 = rows.iter().find(|r| r["id"] == json!("notif-3")).unwrap();
        assert_eq!(notif3["kind"], json!("external_http_dispatch"));

        // other-source notification never appears
        assert!(rows.iter().all(|r| r["id"] != json!("notif-4")));
    }

    #[test]
    fn since_ts_filters_both_queries() {
        let c = db();
        let r = bypassesList(&json!({"since_ts": "2025-12-30 00:00:00"}), &c).unwrap();
        let rows = r.as_array().unwrap();
        assert!(rows.iter().all(|row| js_string(&row["ts"]) >= "2025-12-30 00:00:00".to_string()));
        assert!(rows.iter().any(|row| row["id"] == json!(3)));
        assert!(rows.iter().all(|row| row["id"] != json!(1))); // 2025-12-28, filtered out
        assert!(rows.iter().all(|row| row["id"] != json!("notif-4"))); // other-source, always excluded
    }

    #[test]
    fn limit_clamps() {
        let c = db();
        // limit=3: per-query LIMIT 3 (by id desc) then merge+resort+final slice(0,3)
        let r3 = bypassesList(&json!({"limit": 3}), &c).unwrap();
        assert_eq!(r3.as_array().unwrap().len(), 3);
        // falsy 0 -> Number(0)||100 default -> all 9 rows
        let r0 = bypassesList(&json!({"limit": 0}), &c).unwrap();
        assert_eq!(r0.as_array().unwrap().len(), 9);
        // negative truthy -> Math.max(1, -10) = 1
        let rneg = bypassesList(&json!({"limit": -10}), &c).unwrap();
        assert_eq!(rneg.as_array().unwrap().len(), 1);
        // Each query is independently capped to 1 BY ROW ID desc (not ts) before the final
        // merge+resort: law_violations top-1-by-id is id=6 (ts 2025-12-27, earliest of all —
        // inserted last), notifications top-1-by-id is notif-3 (ts 2026-01-01 07:00). Re-sorted
        // by ts desc, notif-3 wins (row id=5's 2026-01-01 08:00 never reaches this stage).
        assert_eq!(rneg[0]["id"], json!("notif-3"));
        // over 500 -> clamps to 500, still just returns all 9 available
        let rbig = bypassesList(&json!({"limit": 999}), &c).unwrap();
        assert_eq!(rbig.as_array().unwrap().len(), 9);
        // numeric string -> Number("2") = 2
        let rstr = bypassesList(&json!({"limit": "2"}), &c).unwrap();
        assert_eq!(rstr.as_array().unwrap().len(), 2);
        // null input == {} default; explicit nulls behave like missing/falsy
        assert_eq!(bypassesList(&Value::Null, &c).unwrap().as_array().unwrap().len(), 9);
        assert_eq!(bypassesList(&json!({"since_ts": null, "limit": null}), &c).unwrap().as_array().unwrap().len(), 9);
    }

    #[test]
    fn empty_tables_yield_empty_list() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; \
          CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, severity TEXT, source TEXT, message TEXT, related_card_slug TEXT, payload_json TEXT, dismissed_at TEXT); \
          CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT); \
          CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER, attempted_at TEXT, attempted_by TEXT, agent_name TEXT, action_name TEXT, context_json TEXT, outcome TEXT);").unwrap();
        assert_eq!(bypassesList(&json!({}), &c).unwrap(), json!([]));
    }
}
