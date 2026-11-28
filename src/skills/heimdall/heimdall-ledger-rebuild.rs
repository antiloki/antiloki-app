//! heimdall-ledger-rebuild.rs — twin of skills/heimdall/heimdall-ledger-rebuild.mjs (db lane,
//! cross-db `brain`, frozen-clock). NAMES ARE CONTRACT: `heimdallLedgerRebuild` (default — `async`
//! in JS but every await is a sync `context.db` call, so per playbook §async-default-exports this
//! twin is an ordinary sync fn). FULL rebuild (never incremental) of brain.heimdall_ledger from
//! brain.heimdall_evidence: every evidence row inside the window fans out into up to 5 rung cells
//! (template_version → template → category → family → global, keyed by `${model} ${kind} ${ref}`)
//! and each cell gets a Laplace-smoothed (+1/+2) trust-weighted score plus a sample_ok flag (plan
//! §3.1, §5.4). Trust resolution ladder (provenance,category) → (provenance,'') → PROVENANCE_TRUST
//! seed → 0.1, mirroring _helpers/casting.mjs's PROVENANCE_TRUST/SAMPLE_OK — data consts, not
//! units (already-converted casting.rs owns the *functions* it exports; these two objects have no
//! twin of their own, so their values are transcribed here by hand).
//! FROZEN-CLOCK LANE: `datetime('now', ?)` (evidence window) and `datetime('now')` (computed_at)
//! pinned to 2026-01-01 00:00:00.
//! JS `if (!db) throw err.contextMissing(...)` is unreachable in the db lane (a connection is
//! always injected) — not transcribed.
// QUALITY: 80/100 — 5-rung fan-out, Laplace-smoothed trust score, window-scoped full rebuild

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (subset used).
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
// JS Number.prototype.toString() for a raw f64 embedded into a template-literal string (here, the
// `-${window_days} days` SQL modifier) — no exponential notation in this skill's practical range.
fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); }
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k { format!("{digits}{}", "0".repeat((exp + 1 - k) as usize)) }
            else { let (i, f) = digits.split_at((exp + 1) as usize); format!("{i}.{f}") }
        } else { format!("0.{}{digits}", "0".repeat((-exp - 1) as usize)) }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}
// JS `+x.toFixed(4)`.
fn to_fixed4(x: f64) -> f64 { format!("{:.4}", x).parse().unwrap() }
// JSON.stringify(N) for a Number that may or may not be whole (gotcha #2: json!(30.0) renders
// "30.0", but JS `30` renders "30").
fn num_json(f: f64) -> Value { if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 { json!(f as i64) } else { json!(f) } }

// _helpers/casting.mjs PROVENANCE_TRUST (plan §5.2) — data const, not a unit; transcribed by value.
fn provenance_trust(provenance: &str) -> Option<f64> {
    match provenance {
        "human" | "prod_gate" | "prod_terminal" => Some(1.0),
        "prod_court" => Some(0.7),
        "ai_judge" => Some(0.4),
        "ai_panel" => Some(0.6),
        "bench_det" => Some(0.3),
        "bench_rubric" | "playground" => Some(0.2),
        "lineage_prior" => Some(0.15),
        _ => None,
    }
}
// _helpers/casting.mjs SAMPLE_OK (plan §5.4) — every rung but 'global' needs 12 in-window calls;
// global needs 25. `kind` here is always one of the 5 literal rung names this file bumps.
fn sample_ok_min(kind: &str) -> i64 { if kind == "global" { 25 } else { 12 } }

const POSITIVE: [&str; 2] = ["clean", "ship"];

struct Cell {
    model: Value,
    kind: &'static str,
    scope_ref: Value,
    calls: i64,
    clean: i64,
    fails: i64,
    burns: i64,
    ships: i64,
    w_pos: f64,
    w_all: f64,
}

// trustFor(provenance, category): (provenance,category) override → (provenance,'') override →
// PROVENANCE_TRUST[provenance] → 0.1. `trust_ov` keys are pre-built as `${provenance}|${category??''}`
// exactly like the JS Map, so the two lookups here mirror the JS `??` chain (nz = the `??` filter).
fn trust_for(trust_ov: &HashMap<String, Value>, provenance: &Value, category: &Value) -> f64 {
    let prov_str = tmpl(Some(provenance));
    let cat_part = if category.is_null() { String::new() } else { tmpl(Some(category)) };
    let key1 = format!("{prov_str}|{cat_part}");
    let key2 = format!("{prov_str}|");
    trust_ov.get(&key1).and_then(|v| nz(Some(v))).and_then(|v| v.as_f64())
        .or_else(|| trust_ov.get(&key2).and_then(|v| nz(Some(v))).and_then(|v| v.as_f64()))
        .or_else(|| provenance_trust(&prov_str))
        .unwrap_or(0.1)
}

// bump(model, kind, ref, row): fold one evidence row into the (model,kind,ref) cell, creating it
// on first sight. `order` tracks insertion order separately from the HashMap (gotcha #1 — a plain
// HashMap iterates in random order, which would scramble the later INSERT order and diverge
// db_after's row order from the JS Map's insertion-ordered `cells.values()`).
fn bump(
    order: &mut Vec<String>,
    cells: &mut HashMap<String, Cell>,
    model: &Value,
    kind: &'static str,
    scope_ref: &Value,
    row: &Value,
    trust_ov: &HashMap<String, Value>,
) {
    if !truthy(Some(scope_ref)) { return; }
    let key = format!("{} {} {}", tmpl(Some(model)), kind, tmpl(Some(scope_ref)));
    if !cells.contains_key(&key) {
        cells.insert(key.clone(), Cell {
            model: model.clone(), kind, scope_ref: scope_ref.clone(),
            calls: 0, clean: 0, fails: 0, burns: 0, ships: 0, w_pos: 0.0, w_all: 0.0,
        });
        order.push(key.clone());
    }
    let outcome = row.get("outcome").and_then(|v| v.as_str()).unwrap_or("").to_string();
    // trustFor always reads the EVIDENCE ROW's own provenance/category — not the rung's `ref` —
    // so the same trust value applies no matter which of the 5 rungs is being bumped for this row.
    let provenance = row.get("provenance").cloned().unwrap_or(Value::Null);
    let category = row.get("category").cloned().unwrap_or(Value::Null);
    let trust = trust_for(trust_ov, &provenance, &category);
    let c = cells.get_mut(&key).unwrap();
    c.calls += 1;
    match outcome.as_str() {
        "clean" => c.clean += 1,
        "fail" => c.fails += 1,
        "burn" => c.burns += 1,
        "ship" => c.ships += 1,
        _ => {}
    }
    c.w_all += trust;
    if POSITIVE.contains(&outcome.as_str()) { c.w_pos += trust; }
}

pub fn heimdallLedgerRebuild(input: &Value, db: &Connection) -> Result<Value, String> {
    // `Number(input?.window_days ?? 30)` — nz covers the `??` (absent/null both default), js_number
    // covers the Number() coercion (so a stringy "7" input still lands as 7.0).
    let wd_val = nz(input.get("window_days")).cloned().unwrap_or(json!(30));
    let window_days = js_number(Some(&wd_val));

    let window_mod = format!("-{} days", js_number_to_string(window_days));
    let evidence = crate::db_json::query_rows(db, &format!("
        SELECT e.model_slug, e.category, e.template_base, e.provenance, e.outcome,
               c.family, pt.name AS template_version
        FROM brain.heimdall_evidence e
        LEFT JOIN brain.heimdall_categories c ON c.slug = e.category
        LEFT JOIN brain.ai_calls a ON a.id = e.ai_call_id
        LEFT JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id
        WHERE e.ts >= datetime({}, ?)
    ", crate::clock::now_sql_inner()), &[json!(window_mod)])?;
    let evidence_rows = evidence.as_array().cloned().unwrap_or_default();

    // AI-3: committed (provenance × category) trust overrides. Table absent on a legacy brain →
    // swallow (JS try/catch), fall back to the PROVENANCE_TRUST seed for every row.
    let mut trust_ov: HashMap<String, Value> = HashMap::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT provenance, category, trust FROM brain.heimdall_trust_overrides", &[]) {
        if let Some(arr) = rows.as_array() {
            for o in arr {
                let provenance = o.get("provenance").cloned().unwrap_or(Value::Null);
                let category = o.get("category").cloned().unwrap_or(Value::Null);
                let cat_part = if category.is_null() { String::new() } else { tmpl(Some(&category)) };
                let key = format!("{}|{}", tmpl(Some(&provenance)), cat_part);
                trust_ov.insert(key, o.get("trust").cloned().unwrap_or(Value::Null));
            }
        }
    }

    let mut order: Vec<String> = Vec::new();
    let mut cells: HashMap<String, Cell> = HashMap::new();
    for e in &evidence_rows {
        let model = e.get("model_slug").cloned().unwrap_or(Value::Null);
        let template_base = e.get("template_base").cloned().unwrap_or(Value::Null);
        let template_version = e.get("template_version").cloned().unwrap_or(Value::Null);
        let category = e.get("category").cloned().unwrap_or(Value::Null);
        let family = e.get("family").cloned().unwrap_or(Value::Null);

        // finest rung: only when the version name is known AND actually versioned (differs from
        // the base) — a bare base needs no v-rung.
        if truthy(Some(&template_version)) && template_version != template_base {
            bump(&mut order, &mut cells, &model, "template_version", &template_version, e, &trust_ov);
        }
        bump(&mut order, &mut cells, &model, "template", &template_base, e, &trust_ov);
        bump(&mut order, &mut cells, &model, "category", &category, e, &trust_ov);
        bump(&mut order, &mut cells, &model, "family", &family, e, &trust_ov);
        bump(&mut order, &mut cells, &model, "global", &json!("global"), e, &trust_ov);
    }

    let mut cnt_template_version: i64 = 0;
    let mut cnt_template: i64 = 0;
    let mut cnt_category: i64 = 0;
    let mut cnt_family: i64 = 0;
    let mut cnt_global: i64 = 0;

    db.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    let body: Result<(), String> = (|| {
        crate::db_json::run_write(db, "DELETE FROM brain.heimdall_ledger WHERE window_days = ?", &[json!(window_days)])?;
        for key in &order {
            let c = cells.get(key).expect("cell exists (created during fan-out)");
            // Laplace smoothing: +1 positive / +2 total — a tiny sample can't post a perfect score.
            let score = (c.w_pos + 1.0) / (c.w_all + 2.0);
            let min_n = sample_ok_min(c.kind);
            let sample_ok_flag: i64 = if c.calls >= min_n { 1 } else { 0 };
            crate::db_json::run_write(db, &format!("
                INSERT INTO brain.heimdall_ledger
                  (model_slug, scope_kind, scope_ref, window_days, calls, clean, fails, burns, ships, weighted_score, sample_ok, computed_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, {})
            ", crate::clock::now_sql()), &[
                c.model.clone(), json!(c.kind), c.scope_ref.clone(), json!(window_days),
                json!(c.calls), json!(c.clean), json!(c.fails), json!(c.burns), json!(c.ships),
                json!(to_fixed4(score)), json!(sample_ok_flag),
            ])?;
            match c.kind {
                "template_version" => cnt_template_version += 1,
                "template" => cnt_template += 1,
                "category" => cnt_category += 1,
                "family" => cnt_family += 1,
                "global" => cnt_global += 1,
                _ => {}
            }
        }
        db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(e) = body {
        let _ = db.execute_batch("ROLLBACK");
        return Err(e);
    }

    let mut by_rung = Map::new();
    by_rung.insert("template_version".into(), json!(cnt_template_version));
    by_rung.insert("template".into(), json!(cnt_template));
    by_rung.insert("category".into(), json!(cnt_category));
    by_rung.insert("family".into(), json!(cnt_family));
    by_rung.insert("global".into(), json!(cnt_global));

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("window_days".into(), num_json(window_days));
    out.insert("rows".into(), json!(order.len() as i64));
    out.insert("by_rung".into(), Value::Object(by_rung));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_brain() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("
            ATTACH ':memory:' AS brain;
            CREATE TABLE brain.heimdall_evidence (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, model_slug TEXT NOT NULL, category TEXT, template_base TEXT NOT NULL, provenance TEXT NOT NULL, outcome TEXT NOT NULL, ai_call_id INTEGER);
            CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, family TEXT);
            CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, prompt_template_id INTEGER);
            CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT);
            CREATE TABLE brain.heimdall_trust_overrides (provenance TEXT, category TEXT, trust REAL);
            CREATE TABLE brain.heimdall_ledger (model_slug TEXT NOT NULL, scope_kind TEXT NOT NULL, scope_ref TEXT NOT NULL, window_days INTEGER NOT NULL, calls INTEGER NOT NULL DEFAULT 0, clean INTEGER NOT NULL DEFAULT 0, fails INTEGER NOT NULL DEFAULT 0, burns INTEGER NOT NULL DEFAULT 0, ships INTEGER NOT NULL DEFAULT 0, weighted_score REAL, sample_ok INTEGER NOT NULL DEFAULT 0, computed_at TEXT);
        ").unwrap();
        db
    }

    fn ins_evidence(db: &Connection, ts: &str, model: &str, category: Option<&str>, template_base: &str, provenance: &str, outcome: &str, ai_call_id: Option<i64>) {
        db.execute(
            "INSERT INTO brain.heimdall_evidence (ts, model_slug, category, template_base, provenance, outcome, ai_call_id) VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![ts, model, category, template_base, provenance, outcome, ai_call_id],
        ).unwrap();
    }

    #[test]
    fn basic_rebuild_counts_rungs_and_laplace_score() {
        let db = db_with_brain();
        ins_evidence(&db, "2025-12-20 00:00:00", "claude-sonnet-4-6", Some("conversational"), "chat", "human", "clean", None);
        ins_evidence(&db, "2025-12-21 00:00:00", "claude-sonnet-4-6", Some("conversational"), "chat", "human", "fail", None);

        let r = heimdallLedgerRebuild(&json!({}), &db).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["window_days"], json!(30));
        // template, category, global — no categories row so family stays null (skipped); no
        // ai_call_id so template_version stays null (skipped).
        assert_eq!(r["rows"], json!(3));
        assert_eq!(r["by_rung"], json!({ "template_version": 0, "template": 1, "category": 1, "family": 0, "global": 1 }));

        let rows = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_ledger WHERE scope_kind = 'template'", &[]).unwrap();
        let row = &rows[0];
        assert_eq!(row["calls"], json!(2));
        assert_eq!(row["clean"], json!(1));
        assert_eq!(row["fails"], json!(1));
        assert_eq!(row["weighted_score"], json!(0.5)); // (1*1.0 + 1) / (2*1.0 + 2) = 2/4
        assert_eq!(row["sample_ok"], json!(0));
    }

    #[test]
    fn version_rung_only_when_it_differs_from_base_and_trust_override_ladder() {
        let db = db_with_brain();
        db.execute_batch("
            INSERT INTO brain.heimdall_categories (slug, family) VALUES ('conversational', 'LANGUAGE');
            INSERT INTO brain.ai_calls (id, prompt_template_id) VALUES (1, 10), (2, 20);
            INSERT INTO brain.prompt_templates (id, name) VALUES (10, 'chat.v3'), (20, 'chat');
            INSERT INTO brain.heimdall_trust_overrides (provenance, category, trust) VALUES ('ai_panel', NULL, 0.5);
        ").unwrap();
        ins_evidence(&db, "2025-12-20 00:00:00", "m1", Some("conversational"), "chat", "ai_panel", "ship", Some(1));
        ins_evidence(&db, "2025-12-21 00:00:00", "m1", Some("conversational"), "chat", "ai_panel", "ship", Some(2));

        let r = heimdallLedgerRebuild(&json!({}), &db).unwrap();
        assert_eq!(r["by_rung"], json!({ "template_version": 1, "template": 1, "category": 1, "family": 1, "global": 1 }));

        let fam = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_ledger WHERE scope_kind = 'family'", &[]).unwrap();
        assert_eq!(fam[0]["scope_ref"], json!("LANGUAGE"));
        assert_eq!(fam[0]["calls"], json!(2));
        // trust: (ai_panel, category-null) override fallback = 0.5, both rows 'ship' (positive).
        // score = (2*0.5 + 1) / (2*0.5 + 2) = 2/3 = 0.6667
        assert_eq!(fam[0]["weighted_score"], json!(0.6667));

        let ver = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_ledger WHERE scope_kind = 'template_version'", &[]).unwrap();
        assert_eq!(ver[0]["scope_ref"], json!("chat.v3"));
        assert_eq!(ver[0]["calls"], json!(1)); // only the ai_call_id=1 row; version 'chat' == base is skipped
    }

    #[test]
    fn unmapped_provenance_falls_back_to_default_point_one() {
        let db = db_with_brain();
        ins_evidence(&db, "2025-12-20 00:00:00", "m1", None, "chat", "totally-unknown-source", "burn", None);
        let r = heimdallLedgerRebuild(&json!({}), &db).unwrap();
        // category null => category+family rungs skipped; template+global remain.
        assert_eq!(r["rows"], json!(2));
        let row = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_ledger WHERE scope_kind = 'template'", &[]).unwrap();
        // burn isn't positive: wPos=0, wAll=0.1 (default) => score = (0+1)/(0.1+2) = 1/2.1 = 0.4762
        assert_eq!(row[0]["weighted_score"], json!(0.4762));
        assert_eq!(row[0]["burns"], json!(1));
    }

    #[test]
    fn window_filter_and_full_rebuild_only_touches_its_own_window() {
        let db = db_with_brain();
        ins_evidence(&db, "2025-12-28 00:00:00", "m1", None, "chat", "human", "clean", None); // within 7d of frozen now
        ins_evidence(&db, "2025-12-10 00:00:00", "m1", None, "chat", "human", "clean", None); // outside 7d
        db.execute_batch("
            INSERT INTO brain.heimdall_ledger (model_slug, scope_kind, scope_ref, window_days, calls, weighted_score, sample_ok, computed_at) VALUES ('stale','template','chat',7,99,0.99,1,'2020-01-01');
            INSERT INTO brain.heimdall_ledger (model_slug, scope_kind, scope_ref, window_days, calls, weighted_score, sample_ok, computed_at) VALUES ('other-window','template','chat',30,5,0.5,0,'2020-01-01');
        ").unwrap();

        let r = heimdallLedgerRebuild(&json!({ "window_days": 7 }), &db).unwrap();
        assert_eq!(r["window_days"], json!(7));
        assert_eq!(r["rows"], json!(2)); // template + global; category null => skipped

        let rows7 = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_ledger WHERE window_days = 7", &[]).unwrap();
        assert_eq!(rows7.as_array().unwrap().len(), 2);
        assert!(rows7.as_array().unwrap().iter().all(|x| x["model_slug"] != json!("stale")));
        let tmpl_row = rows7.as_array().unwrap().iter().find(|x| x["scope_kind"] == json!("template")).unwrap();
        assert_eq!(tmpl_row["calls"], json!(1)); // only the within-window row

        let rows30 = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_ledger WHERE window_days = 30", &[]).unwrap();
        assert_eq!(rows30.as_array().unwrap().len(), 1);
        assert_eq!(rows30[0]["model_slug"], json!("other-window")); // untouched by the window=7 rebuild
    }

    #[test]
    fn global_rung_crosses_sample_ok_at_25_calls_while_finer_rungs_stay_below() {
        let db = db_with_brain();
        for i in 0..25 {
            // distinct template_base per row keeps every `template` cell at 1 call (never
            // sample_ok), while every row shares the same model so `global` accumulates all 25.
            ins_evidence(&db, "2025-12-20 00:00:00", "m1", None, &format!("tpl-{i}"), "human", "clean", None);
        }
        let r = heimdallLedgerRebuild(&json!({}), &db).unwrap();
        assert_eq!(r["by_rung"]["template"], json!(25)); // 25 distinct template cells
        assert_eq!(r["by_rung"]["global"], json!(1)); // 1 cell (same model), all 25 calls

        let g = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_ledger WHERE scope_kind = 'global'", &[]).unwrap();
        assert_eq!(g[0]["calls"], json!(25));
        assert_eq!(g[0]["sample_ok"], json!(1)); // 25 >= 25

        let t = crate::db_json::query_rows(&db, "SELECT * FROM brain.heimdall_ledger WHERE scope_kind = 'template' AND scope_ref = 'tpl-0'", &[]).unwrap();
        assert_eq!(t[0]["calls"], json!(1));
        assert_eq!(t[0]["sample_ok"], json!(0)); // 1 < 12
    }
}
