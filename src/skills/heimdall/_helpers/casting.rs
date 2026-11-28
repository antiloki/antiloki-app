//! casting.rs — twin of skills/heimdall/_helpers/casting.mjs (cross-db helper module: attaches
//! `brain`). NAMES ARE CONTRACT: judgeEstTokens · templateBase · classifyForCall · ladderWalk ·
//! ensureHeimdallModes · heimdallModeFor · setHeimdallMode · heimdallRank1 · recordCasting —
//! HEIMDALL's deterministic resolution core (plan §5.4 ladder walk, §5.7-bis trace).
//! FROZEN-CLOCK LANE: ensureHeimdallModes executes DDL with a `strftime(...,'now')` column
//! DEFAULT — pinned to the frozen instant 2026-01-01 00:00:00 (playbook DDL rule) so a later
//! INSERT that relies on the default fires identically in both lanes.
// QUALITY: 80/100 — cross-db ladder walk (template→category→family→global) + telemetry writer
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

// Standard helper functions (playbook §Standard helper functions) — copied verbatim.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

fn template_base_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\.v\d+$").unwrap())
}

// judgeEstTokens({ tokens_in = 0, tokens_out = 0, panel = false }) — no-db. Destructure defaults
// only fire on `undefined`, but `Number(x) || 0` maps NaN/0/undefined alike to 0.0, so treating
// an absent field as 0 up front is equivalent for every JSON-representable input.
pub fn judgeEstTokens(a: &Value) -> Value {
    let ti = js_number(a.get("tokens_in"));
    let ti = if ti.is_nan() { 0.0 } else { ti };
    let to = js_number(a.get("tokens_out"));
    let to = if to.is_nan() { 0.0 } else { to };
    let panel = truthy(a.get("panel"));
    let one = 700.0 + ti.min(2000.0) + to.min(2000.0) + 550.0;
    json!((if panel { one * 3.0 } else { one }) as i64)
}

pub fn templateBase(name: &Value) -> Value {
    let s = str_qq(Some(name));
    json!(template_base_re().replace(&s, "").to_string())
}

// Deterministic classification — mirrors heimdall-classify's lookup, inlined here (source
// comment: "never imports a dispatchable skill"). Cross-db: reads brain.heimdall_template_categories
// + brain.heimdall_categories via the attached `brain` db.
pub fn classifyForCall(db: &Connection, template_name: &Value) -> Value {
    let base = templateBase(template_name);
    let base_str = base.as_str().unwrap_or("").to_string();
    let map_rows = crate::db_json::query_rows(
        db,
        "SELECT category FROM brain.heimdall_template_categories WHERE template_base = ?",
        &[json!(base_str)],
    ).unwrap_or_else(|_| json!([]));
    let map = map_rows.as_array().and_then(|a| a.first()).cloned();
    let slug = map.as_ref()
        .and_then(|m| m.get("category"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!("general"));
    let slug_str = slug.as_str().unwrap_or("general").to_string();
    let cat_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, family, weight_profile FROM brain.heimdall_categories WHERE slug = ?",
        &[json!(slug_str)],
    ).unwrap_or_else(|_| json!([]));
    let cat = cat_rows.as_array().and_then(|a| a.first()).cloned()
        .unwrap_or(json!({ "slug": "general", "family": "LANGUAGE", "weight_profile": "TALKER" }));
    let mut out = Map::new();
    out.insert("category".into(), cat.get("slug").cloned().unwrap_or(Value::Null));
    out.insert("family".into(), cat.get("family").cloned().unwrap_or(Value::Null));
    out.insert("weight_profile".into(), cat.get("weight_profile").cloned().unwrap_or(Value::Null));
    out.insert("template_base".into(), json!(base_str));
    out.insert("source".into(), json!(if map.is_some() { "template-map" } else { "unmapped" }));
    out.insert("confidence".into(), json!(if map.is_some() { 1 } else { 0 }));
    Value::Object(out)
}

// Walk the specificity ladder (template → category → family → global). Resolution = first rung
// with >=1 sample_ok candidate. Pure cross-db read over brain.heimdall_ledger.
pub fn ladderWalk(db: &Connection, a: &Value) -> Value {
    let template_base = a.get("template_base").cloned().unwrap_or(Value::Null);
    let category = a.get("category").cloned().unwrap_or(Value::Null);
    let family = a.get("family").cloned().unwrap_or(Value::Null);
    // `window_days = 30` (destructure default) fires only on undefined — an absent key. A key
    // present as explicit null is NOT defaulted in JS; mirror that (missing => 30, present => as-is).
    let window_days = match a.get("window_days") { None => json!(30), Some(v) => v.clone() };

    let rungs: [(&str, Value); 4] = [
        ("template", template_base),
        ("category", category),
        ("family", family),
        ("global", json!("global")),
    ];
    let mut walk: Vec<Value> = Vec::new();
    let mut resolved: Option<Value> = None;
    for (rung, scope_ref) in rungs {
        if !truthy(Some(&scope_ref)) { continue; }
        let rows = crate::db_json::query_rows(db, "
            SELECT model_slug, calls, clean, fails, burns, ships, weighted_score, sample_ok
            FROM brain.heimdall_ledger
            WHERE scope_kind = ? AND scope_ref = ? AND window_days = ?
            ORDER BY weighted_score DESC, calls DESC
        ", &[json!(rung), scope_ref.clone(), window_days.clone()]).unwrap_or_else(|_| json!([]));
        let rows_arr = rows.as_array().cloned().unwrap_or_default();
        let ok: Vec<Value> = rows_arr.iter()
            .filter(|x| x.get("sample_ok").and_then(|v| v.as_i64()) == Some(1))
            .cloned().collect();
        walk.push(json!({ "rung": rung, "scope_ref": scope_ref, "candidates": rows_arr.len() as i64, "sample_ok": !ok.is_empty() }));
        if resolved.is_none() && !ok.is_empty() {
            resolved = Some(json!({ "rung": rung, "scope_ref": scope_ref, "rows": ok }));
        }
    }
    json!({ "walk": walk, "resolved": resolved })
}

// PER-TEMPLATE ROUTING MODE table (operator 2026-07-19). Idempotent create — a fn that EXECUTES
// DDL, so the strftime(...,'now') DEFAULT is pinned to the frozen instant (playbook DDL rule):
// a later INSERT (setHeimdallMode) that relies on this default must fire the SAME literal both
// lanes. The JS `_modesReady` WeakSet is a per-process memo only — CREATE TABLE IF NOT EXISTS is
// already idempotent, so the twin has no cache and always issues the (no-op after first) DDL.
pub fn ensureHeimdallModes(db: &Connection) -> Value {
    let _ = db.execute_batch(&format!("CREATE TABLE IF NOT EXISTS brain.heimdall_modes (
      template_base TEXT PRIMARY KEY,
      mode TEXT NOT NULL CHECK (mode IN ('train', 'rank1')),
      updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', {}))
    )", crate::clock::now_sql_inner()));
    Value::Null
}

pub fn heimdallModeFor(db: &Connection, template_name: &Value) -> Value {
    ensureHeimdallModes(db);
    let base = templateBase(template_name);
    let base_str = base.as_str().unwrap_or("").to_string();
    let rows = crate::db_json::query_rows(db, "SELECT mode FROM brain.heimdall_modes WHERE template_base = ?", &[json!(base_str)]);
    match rows {
        Ok(v) => v.as_array().and_then(|a| a.first())
            .and_then(|r| r.get("mode"))
            .filter(|x| !x.is_null())
            .cloned()
            .unwrap_or(Value::Null),
        Err(_) => Value::Null,
    }
}

pub fn setHeimdallMode(db: &Connection, template_base: &Value, mode: &Value) -> Value {
    ensureHeimdallModes(db);
    let base = templateBase(template_base);
    let base_str = base.as_str().unwrap_or("").to_string();
    if mode.is_null() || mode.as_str() == Some("default") {
        let _ = db.execute("DELETE FROM brain.heimdall_modes WHERE template_base = ?", rusqlite::params![base_str]);
        return json!({ "template_base": base_str, "mode": null });
    }
    let mode_str = mode.as_str().unwrap_or("");
    if mode_str != "train" && mode_str != "rank1" {
        // Not fixtured (playbook gotcha 9 — only the success path is fixtured): the JS throws
        // here with no surrounding try/catch in setHeimdallMode itself, so a caller passing an
        // invalid mode is a contract violation, not a value this twin needs to represent as data.
        panic!("heimdall mode must be train|rank1|default, got: {}", tmpl(Some(mode)));
    }
    let _ = db.execute(
        &format!("INSERT INTO brain.heimdall_modes (template_base, mode) VALUES (?, ?)
         ON CONFLICT(template_base) DO UPDATE SET mode = excluded.mode, updated_at = strftime('%Y-%m-%d %H:%M:%f', {})", crate::clock::now_sql_inner()),
        rusqlite::params![base_str, mode_str],
    );
    json!({ "template_base": base_str, "mode": mode_str })
}

// The ranking-1 model for a template — the ladder's first sample-ok candidate. null when no rung
// has statistical footing yet (the honest cold-start answer). classifyForCall/ladderWalk already
// swallow their own db errors internally, so the JS try/catch here has nothing left to catch.
pub fn heimdallRank1(db: &Connection, template_name: &Value) -> Value {
    let classify = classifyForCall(db, template_name);
    let ladder_input = json!({
        "template_base": classify.get("template_base").cloned().unwrap_or(Value::Null),
        "category": classify.get("category").cloned().unwrap_or(Value::Null),
        "family": classify.get("family").cloned().unwrap_or(Value::Null),
    });
    let walk_result = ladderWalk(db, &ladder_input);
    walk_result.get("resolved")
        .filter(|r| !r.is_null())
        .and_then(|r| r.get("rows"))
        .and_then(|rows| rows.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("model_slug"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(Value::Null)
}

// Record one casting + its full trace. Telemetry NEVER throws into the call path: any DB failure
// (e.g. an invalid `reason` tripping the heimdall_castings CHECK constraint) is swallowed to null,
// mirroring the JS try/catch around the whole function body.
pub fn recordCasting(db: &Connection, a: &Value) -> Value {
    let classify = classifyForCall(db, a.get("template_name").unwrap_or(&Value::Null));
    let ladder_input = json!({
        "template_base": classify.get("template_base").cloned().unwrap_or(Value::Null),
        "category": classify.get("category").cloned().unwrap_or(Value::Null),
        "family": classify.get("family").cloned().unwrap_or(Value::Null),
    });
    let ladder = ladderWalk(db, &ladder_input);
    let walk = ladder.get("walk").cloned().unwrap_or(json!([]));
    let resolved = ladder.get("resolved").cloned().filter(|v| !v.is_null());

    let resolved_rows: Vec<Value> = resolved.as_ref()
        .and_then(|r| r.get("rows"))
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let scores: Vec<Value> = resolved_rows.iter().take(6).map(|r| json!({
        "model": r.get("model_slug").cloned().unwrap_or(Value::Null),
        "blended": r.get("weighted_score").cloned().unwrap_or(Value::Null),
        "evidence": {
            "calls": r.get("calls").cloned().unwrap_or(Value::Null),
            "clean": r.get("clean").cloned().unwrap_or(Value::Null),
            "fails": r.get("fails").cloned().unwrap_or(Value::Null),
            "burns": r.get("burns").cloned().unwrap_or(Value::Null),
            "ships": r.get("ships").cloned().unwrap_or(Value::Null),
            "window_days": 30,
        },
    })).collect();

    // floor.requested_tier: absent `tier` key stays OMITTED (JS: JSON.stringify drops an
    // undefined-valued property); an explicit null is kept.
    let mut floor = Map::new();
    if let Some(tier_v) = a.get("tier") { floor.insert("requested_tier".into(), tier_v.clone()); }
    let tier_source = nz(a.get("tier_source")).cloned().unwrap_or(json!("route/template"));
    floor.insert("resolved_by".into(), tier_source);

    let model_id = nz(a.get("model_id")).cloned().unwrap_or(Value::Null);
    let provider = nz(a.get("provider")).cloned().unwrap_or(Value::Null);
    // `reason = 'observe'` destructure default fires only on undefined (absent key); explicit
    // null is NOT defaulted (mirrors ladderWalk's window_days handling above).
    let reason_v = match a.get("reason") { None => json!("observe"), Some(v) => v.clone() };
    let rung = resolved.as_ref()
        .and_then(|r| r.get("rung"))
        .cloned()
        .filter(|v| !v.is_null())
        .unwrap_or(Value::Null);

    let mut decision = Map::new();
    decision.insert("effective_model".into(), model_id.clone());
    decision.insert("provider".into(), provider);
    decision.insert("reason".into(), reason_v.clone());
    decision.insert("rung".into(), rung);

    let mut trace = Map::new();
    trace.insert("classify".into(), classify.clone());
    trace.insert("floor".into(), Value::Object(floor));
    trace.insert("ladder".into(), walk);
    trace.insert("filters".into(), json!([]));
    trace.insert("scores".into(), json!(scores));
    trace.insert("explore".into(), Value::Null);
    trace.insert("decision".into(), Value::Object(decision));
    let trace_json = serde_json::to_string(&Value::Object(trace)).unwrap_or_else(|_| "null".into());

    let ai_call_id = nz(a.get("ai_call_id")).cloned().unwrap_or(Value::Null);
    let template_base = classify.get("template_base").cloned().unwrap_or(Value::Null);
    let category = classify.get("category").cloned().unwrap_or(Value::Null);
    let requested_tier = str_qq(a.get("tier"));
    let reason_str = js_string(&reason_v);

    let sql = "INSERT INTO brain.heimdall_castings
        (ai_call_id, template_base, category, requested_tier, effective_model, reason, trace_json)
      VALUES (?, ?, ?, ?, ?, ?, ?)";
    match crate::db_json::run_write(db, sql, &[
        ai_call_id, template_base, category, json!(requested_tier), model_id, json!(reason_str), json!(trace_json),
    ]) {
        Ok((_changes, rowid)) => json!(rowid),
        Err(_) => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_taxonomy(db: &Connection) {
        db.execute_batch("
            ATTACH ':memory:' AS brain;
            CREATE TABLE brain.heimdall_template_categories (template_base TEXT PRIMARY KEY, category TEXT NOT NULL);
            CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, family TEXT NOT NULL, weight_profile TEXT NOT NULL);
            CREATE TABLE brain.heimdall_ledger (model_slug TEXT NOT NULL, scope_kind TEXT NOT NULL, scope_ref TEXT NOT NULL, window_days INTEGER NOT NULL, calls INTEGER NOT NULL DEFAULT 0, clean INTEGER NOT NULL DEFAULT 0, fails INTEGER NOT NULL DEFAULT 0, burns INTEGER NOT NULL DEFAULT 0, ships INTEGER NOT NULL DEFAULT 0, weighted_score REAL, sample_ok INTEGER NOT NULL DEFAULT 0);
            INSERT INTO brain.heimdall_template_categories (template_base, category) VALUES ('chat', 'conversational');
            INSERT INTO brain.heimdall_categories (slug, family, weight_profile) VALUES ('conversational', 'LANGUAGE', 'TALKER');
            INSERT INTO brain.heimdall_ledger VALUES ('claude-sonnet-4-6','template','chat',30,50,40,5,2,3,0.82,1);
            INSERT INTO brain.heimdall_ledger VALUES ('gpt-5','template','chat',30,5,2,1,0,0,0.55,0);
        ").unwrap();
    }

    #[test]
    fn judge_est_tokens_defaults_caps_and_panel_triples() {
        assert_eq!(judgeEstTokens(&json!({})), json!(1250));
        assert_eq!(judgeEstTokens(&json!({ "tokens_in": 100, "tokens_out": 200 })), json!(1550));
        assert_eq!(judgeEstTokens(&json!({ "tokens_in": 5000, "tokens_out": 3000 })), json!(5250));
        assert_eq!(judgeEstTokens(&json!({ "tokens_in": 300, "tokens_out": 100, "panel": true })), json!(4950));
    }

    #[test]
    fn template_base_strips_trailing_version() {
        assert_eq!(templateBase(&json!("chat.v11")), json!("chat"));
        assert_eq!(templateBase(&json!("chat")), json!("chat"));
        assert_eq!(templateBase(&Value::Null), json!(""));
    }

    #[test]
    fn classify_maps_falls_back_and_defaults() {
        let db = Connection::open_in_memory().unwrap();
        seed_taxonomy(&db);
        assert_eq!(classifyForCall(&db, &json!("chat.v11")), json!({
            "category": "conversational", "family": "LANGUAGE", "weight_profile": "TALKER",
            "template_base": "chat", "source": "template-map", "confidence": 1
        }));
        assert_eq!(classifyForCall(&db, &json!("unknown.v3")), json!({
            "category": "general", "family": "LANGUAGE", "weight_profile": "TALKER",
            "template_base": "unknown", "source": "unmapped", "confidence": 0
        }));
    }

    #[test]
    fn ladder_walk_resolves_first_sample_ok_rung() {
        let db = Connection::open_in_memory().unwrap();
        seed_taxonomy(&db);
        let r = ladderWalk(&db, &json!({ "template_base": "chat", "category": "conversational", "family": "LANGUAGE" }));
        assert_eq!(r["resolved"]["rung"], json!("template"));
        assert_eq!(r["resolved"]["rows"].as_array().unwrap().len(), 1);
        let none = ladderWalk(&db, &json!({ "template_base": "ghost", "category": "ghost2", "family": "ghost3" }));
        assert_eq!(none["resolved"], Value::Null);
    }

    #[test]
    fn modes_round_trip_and_clear() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        assert_eq!(heimdallModeFor(&db, &json!("chat.v11")), Value::Null);
        let set = setHeimdallMode(&db, &json!("chat.v11"), &json!("train"));
        assert_eq!(set, json!({ "template_base": "chat", "mode": "train" }));
        assert_eq!(heimdallModeFor(&db, &json!("chat.v11")), json!("train"));
        let cleared = setHeimdallMode(&db, &json!("chat.v11"), &Value::Null);
        assert_eq!(cleared, json!({ "template_base": "chat", "mode": null }));
        assert_eq!(heimdallModeFor(&db, &json!("chat.v11")), Value::Null);
    }

    #[test]
    fn rank1_and_record_casting() {
        let db = Connection::open_in_memory().unwrap();
        seed_taxonomy(&db);
        db.execute_batch("CREATE TABLE brain.heimdall_castings (id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, template_base TEXT, category TEXT NOT NULL, requested_tier TEXT NOT NULL, effective_model TEXT, reason TEXT NOT NULL CHECK (reason IN ('observe','override','ledger','probe','quarantine','passthrough','floor_raise')), trace_json TEXT NOT NULL);").unwrap();

        assert_eq!(heimdallRank1(&db, &json!("chat.v11")), json!("claude-sonnet-4-6"));
        assert_eq!(heimdallRank1(&db, &json!("ghost.v1")), Value::Null);

        let id = recordCasting(&db, &json!({
            "ai_call_id": 42, "template_name": "chat.v11", "tier": "mid", "tier_source": "route",
            "model_id": "claude-sonnet-4-6", "provider": "anthropic", "reason": "observe",
        }));
        assert_eq!(id, json!(1));
        let row = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_castings WHERE id = ?", &[id]).unwrap();
        let row = &row[0];
        assert_eq!(row["requested_tier"], json!("mid"));
        assert_eq!(row["category"], json!("conversational"));
        let trace: Value = serde_json::from_str(row["trace_json"].as_str().unwrap()).unwrap();
        assert_eq!(trace["decision"]["effective_model"], json!("claude-sonnet-4-6"));
        assert_eq!(trace["floor"]["requested_tier"], json!("mid"));

        // minimal input: defaults apply, requested_tier column becomes '', floor.requested_tier omitted
        let id2 = recordCasting(&db, &json!({ "template_name": "unknown.v9" }));
        assert_eq!(id2, json!(2));
        let row2 = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_castings WHERE id = ?", &[id2]).unwrap();
        assert_eq!(row2[0]["requested_tier"], json!(""));
        assert_eq!(row2[0]["ai_call_id"], Value::Null);
        let trace2: Value = serde_json::from_str(row2[0]["trace_json"].as_str().unwrap()).unwrap();
        assert!(trace2["floor"].as_object().unwrap().get("requested_tier").is_none());

        // telemetry never throws: an invalid `reason` trips the CHECK constraint -> null, not a panic
        let id3 = recordCasting(&db, &json!({ "template_name": "chat.v11", "reason": "not-a-real-reason" }));
        assert_eq!(id3, Value::Null);
    }
}
