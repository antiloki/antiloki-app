//! score/autonomy-score.rs — twin of skills/score/autonomy-score.mjs (db lane, cross-db:
//! laws.law_violations). NAMES ARE CONTRACT: `autonomyScore` (default), `computeContractScore`,
//! `firstPassFlag` (named). v1 autonomy score: Σ(size_weight × complexity × risk × first_pass_flag)
//! over closed contracts in a window; first_pass requires attempt 1, no supersede, no violation.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

fn size_weight(size: Option<&str>) -> i64 {
    match size {
        Some("XS") => 1,
        Some("S") => 2,
        Some("M") => 3,
        Some("L") => 5,
        _ => 0,
    }
}

pub fn computeContractScore(c: &Value) -> Value {
    let sw = size_weight(c.get("size").and_then(|v| v.as_str()));
    let complexity = c.get("complexity").and_then(|v| v.as_i64()).unwrap_or(0);
    let risk = c.get("risk").and_then(|v| v.as_i64()).unwrap_or(0);
    json!(sw * complexity * risk)
}

// Violations link via context_json (free-form) — accept either the contract slug or the parent
// card_slug appearing in any escaped/blocked outcome. Conservative: either match counts.
fn has_violations_for_contract(db: &Connection, contract_slug: &str, contract_card_slug: Option<&str>) -> bool {
    let rows = match crate::db_json::query_rows(db, "SELECT context_json FROM laws.law_violations WHERE outcome IN ('escaped','blocked')", &[]) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let card_truthy = contract_card_slug.map_or(false, |s| !s.is_empty());
    rows.as_array().map(|arr| arr.iter().any(|r| {
        match r.get("context_json").and_then(|v| v.as_str()) {
            Some(ctx) => ctx.contains(contract_slug) || (card_truthy && ctx.contains(contract_card_slug.unwrap())),
            None => false,
        }
    })).unwrap_or(false)
}

pub fn firstPassFlag(db: &Connection, c: &Value) -> Value {
    let current_attempt = c.get("current_attempt").and_then(|v| v.as_i64()).unwrap_or(1);
    if current_attempt > 1 { return json!(0); }
    if truthy(c.get("superseded_by")) { return json!(0); }
    let slug = c.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    let card_slug = c.get("card_slug").and_then(|v| v.as_str());
    if has_violations_for_contract(db, slug, card_slug) { return json!(0); }
    json!(1)
}

pub fn autonomyScore(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('autonomy-score','db') when no db — the db lane always injects one.
    let since_raw = input.get("since");
    let until_raw = input.get("until");
    let include_details = truthy(input.get("include_details"));
    let details_limit = input.get("details_limit").and_then(|v| v.as_i64()).unwrap_or(50).max(0) as usize;

    let mut wheres = vec!["status = 'done'".to_string()];
    let mut params: Vec<Value> = Vec::new();
    if truthy(since_raw) { wheres.push("updated_at >= ?".to_string()); params.push(since_raw.unwrap().clone()); }
    if truthy(until_raw) { wheres.push("updated_at <= ?".to_string()); params.push(until_raw.unwrap().clone()); }

    let sql = format!(
        "SELECT slug, card_slug, title, status, size, complexity, risk, tier, current_attempt, superseded_by, signed_by, updated_at, created_at FROM contracts WHERE {} ORDER BY updated_at DESC",
        wheres.join(" AND ")
    );
    let contracts = crate::db_json::query_rows(db, &sql, &params)?;
    let contracts = contracts.as_array().cloned().unwrap_or_default();

    let mut rows: Vec<Value> = Vec::new();
    let mut fps: Vec<i64> = Vec::new();
    let mut pts: Vec<i64> = Vec::new();
    let mut tiers: Vec<Value> = Vec::new();
    for c in &contracts {
        let score = computeContractScore(c);
        let first_pass = firstPassFlag(db, c);
        let fp = first_pass.as_i64().unwrap_or(0);
        let points = score.as_i64().unwrap_or(0) * fp;

        let mut row = Map::new();
        row.insert("slug".into(), c.get("slug").cloned().unwrap_or(Value::Null));
        row.insert("tier".into(), c.get("tier").cloned().unwrap_or(Value::Null));
        row.insert("size".into(), c.get("size").cloned().unwrap_or(Value::Null));
        row.insert("complexity".into(), c.get("complexity").cloned().unwrap_or(Value::Null));
        row.insert("risk".into(), c.get("risk").cloned().unwrap_or(Value::Null));
        row.insert("contract_score".into(), score);
        row.insert("first_pass".into(), first_pass);
        row.insert("points".into(), json!(points));
        row.insert("current_attempt".into(), c.get("current_attempt").cloned().unwrap_or(Value::Null));
        row.insert("superseded_by".into(), c.get("superseded_by").cloned().unwrap_or(Value::Null));
        row.insert("updated_at".into(), c.get("updated_at").cloned().unwrap_or(Value::Null));

        tiers.push(c.get("tier").cloned().unwrap_or(Value::Null));
        fps.push(fp);
        pts.push(points);
        rows.push(Value::Object(row));
    }

    let first_pass_count: i64 = fps.iter().sum();
    let period_score: i64 = pts.iter().sum();

    // by_tier: bucket key insertion order = first-seen tier while walking rows (already
    // ORDER BY updated_at DESC), mirroring JS `buckets[t] ??= {...}` object-literal ordering.
    let mut tier_order: Vec<String> = Vec::new();
    let mut tier_buckets: HashMap<String, (i64, i64, i64)> = HashMap::new();
    for i in 0..rows.len() {
        let t = match &tiers[i] { Value::String(s) => s.clone(), _ => "unknown".to_string() };
        if !tier_buckets.contains_key(&t) { tier_order.push(t.clone()); tier_buckets.insert(t.clone(), (0, 0, 0)); }
        let e = tier_buckets.get_mut(&t).unwrap();
        e.0 += 1;
        e.1 += fps[i];
        e.2 += pts[i];
    }
    let mut by_tier = Map::new();
    for t in &tier_order {
        let (closed, first_pass, points) = tier_buckets[t];
        by_tier.insert(t.clone(), json!({ "closed": closed, "first_pass": first_pass, "points": points }));
    }

    let mut streak = 0i64;
    for fp in &fps { if *fp == 1 { streak += 1; } else { break; } }

    let ratio = if rows.is_empty() { json!(0) } else {
        let x = first_pass_count as f64 / rows.len() as f64;
        json!((x * 1000.0).round() / 1000.0)
    };

    let mut out = Map::new();
    out.insert("window".into(), json!({
        "since": nz(since_raw).cloned().unwrap_or(json!("all")),
        "until": nz(until_raw).cloned().unwrap_or(json!("now")),
    }));
    out.insert("contracts_closed".into(), json!(rows.len() as i64));
    out.insert("first_pass_count".into(), json!(first_pass_count));
    out.insert("ratio".into(), ratio);
    out.insert("period_score".into(), json!(period_score));
    out.insert("streak".into(), json!(streak));
    out.insert("by_tier".into(), Value::Object(by_tier));
    out.insert("formula".into(), json!("period_score = Σ (size_weight × complexity × risk × first_pass_flag) for closed contracts in window"));
    out.insert("size_weight_table".into(), json!({ "XS": 1, "S": 2, "M": 3, "L": 5 }));
    if include_details {
        let details: Vec<Value> = rows.into_iter().take(details_limit).collect();
        out.insert("details".into(), json!(details));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;
            CREATE TABLE contracts (slug TEXT NOT NULL UNIQUE, card_slug TEXT, title TEXT, status TEXT NOT NULL DEFAULT 'proposed', size TEXT, complexity INTEGER, risk INTEGER, tier TEXT, current_attempt INTEGER NOT NULL DEFAULT 1, superseded_by TEXT, signed_by TEXT, updated_at TEXT, created_at TEXT);
            CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, context_json TEXT, outcome TEXT);
            INSERT INTO contracts (slug,card_slug,title,status,size,complexity,risk,tier,current_attempt,superseded_by,signed_by,updated_at,created_at) VALUES
              ('SLUG-1','CARD-1','t1','done','L',4,3,'high-risk',1,NULL,NULL,'2026-01-10','2026-01-01'),
              ('SLUG-2','CARD-2','t2','done','M',2,2,'high-risk',1,NULL,NULL,'2026-01-09','2026-01-01'),
              ('SLUG-3','CARD-3','t3','done','S',3,1,'standard',2,NULL,NULL,'2026-01-08','2026-01-01'),
              ('SLUG-4','CARD-4','t4','done','XS',1,1,'standard',1,'SLUG-4B',NULL,'2026-01-07','2026-01-01'),
              ('SLUG-5',NULL,'t5','done','L',5,5,NULL,1,NULL,NULL,'2026-01-06','2026-01-01'),
              ('SLUG-6','CARD-6','t6','done',NULL,NULL,NULL,'trivial',1,NULL,NULL,'2026-01-05','2026-01-01');
            INSERT INTO laws.law_violations (id,context_json,outcome) VALUES
              (1,'{\"contract_slug\":\"SLUG-2\",\"note\":\"threshold\"}','escaped'),
              (2,NULL,'blocked'),
              (3,'{\"whatever\":\"SLUG-99\"}','warned'),
              (4,'{\"card\":\"CARD-999\"}','blocked');").unwrap();
        c
    }

    #[test]
    fn default_window_scores_tiers_and_streak() {
        let r = autonomyScore(&json!({}), &seeded()).unwrap();
        assert_eq!(r["contracts_closed"], json!(6));
        assert_eq!(r["first_pass_count"], json!(3)); // SLUG-1, SLUG-5, SLUG-6
        assert_eq!(r["period_score"], json!(185)); // 60 + 0 + 0 + 0 + 125 + 0
        assert_eq!(r["ratio"], json!(0.5));
        assert_eq!(r["streak"], json!(1)); // SLUG-1 fp=1, SLUG-2 fp=0 breaks it
        assert_eq!(r["window"], json!({ "since": "all", "until": "now" }));
        assert_eq!(r["by_tier"], json!({
            "high-risk": { "closed": 2, "first_pass": 1, "points": 60 },
            "standard": { "closed": 2, "first_pass": 0, "points": 0 },
            "unknown": { "closed": 1, "first_pass": 1, "points": 125 },
            "trivial": { "closed": 1, "first_pass": 1, "points": 0 },
        }));
        assert!(r.get("details").is_none()); // include_details omitted → key absent
    }

    #[test]
    fn since_until_window_and_empty_string_since() {
        let r = autonomyScore(&json!({ "since": "2026-01-07", "until": "2026-01-09" }), &seeded()).unwrap();
        assert_eq!(r["contracts_closed"], json!(3)); // SLUG-2, SLUG-3, SLUG-4
        assert_eq!(r["first_pass_count"], json!(0));
        assert_eq!(r["window"], json!({ "since": "2026-01-07", "until": "2026-01-09" }));

        // empty string is not nullish (window echoes it) but IS falsy (no WHERE filter added)
        let r2 = autonomyScore(&json!({ "since": "" }), &seeded()).unwrap();
        assert_eq!(r2["contracts_closed"], json!(6));
        assert_eq!(r2["window"]["since"], json!(""));
    }

    #[test]
    fn include_details_slices_by_limit() {
        let r = autonomyScore(&json!({ "include_details": true, "details_limit": 2 }), &seeded()).unwrap();
        let details = r["details"].as_array().unwrap();
        assert_eq!(details.len(), 2);
        assert_eq!(details[0]["slug"], json!("SLUG-1"));
        assert_eq!(details[1]["slug"], json!("SLUG-2"));
        assert_eq!(details[0]["contract_score"], json!(60));
    }

    #[test]
    fn empty_result_zeros_everything() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;
            CREATE TABLE contracts (slug TEXT NOT NULL UNIQUE, card_slug TEXT, title TEXT, status TEXT NOT NULL DEFAULT 'proposed', size TEXT, complexity INTEGER, risk INTEGER, tier TEXT, current_attempt INTEGER NOT NULL DEFAULT 1, superseded_by TEXT, signed_by TEXT, updated_at TEXT, created_at TEXT);
            CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, context_json TEXT, outcome TEXT);
            INSERT INTO contracts (slug,card_slug,title,status,updated_at,created_at) VALUES ('S','C','t','doing','2026-01-01','2026-01-01');").unwrap();
        let r = autonomyScore(&json!({}), &c).unwrap();
        assert_eq!(r["contracts_closed"], json!(0));
        assert_eq!(r["first_pass_count"], json!(0));
        assert_eq!(r["period_score"], json!(0));
        assert_eq!(r["ratio"], json!(0));
        assert_eq!(r["streak"], json!(0));
        assert_eq!(r["by_tier"], json!({}));
    }

    #[test]
    fn compute_contract_score_defaults_missing_to_zero() {
        assert_eq!(computeContractScore(&json!({ "size": "S", "complexity": 3, "risk": 4 })), json!(24));
        assert_eq!(computeContractScore(&json!({ "size": "L", "complexity": 5, "risk": 5 })), json!(125));
        assert_eq!(computeContractScore(&json!({})), json!(0));
        assert_eq!(computeContractScore(&json!({ "size": "S" })), json!(0));
        assert_eq!(computeContractScore(&json!({ "size": "Z", "complexity": 2, "risk": 2 })), json!(0));
    }

    #[test]
    fn first_pass_flag_branches() {
        let c = seeded();
        assert_eq!(firstPassFlag(&c, &json!({ "current_attempt": 2, "superseded_by": null, "slug": "X" })), json!(0));
        assert_eq!(firstPassFlag(&c, &json!({ "current_attempt": 1, "superseded_by": "OTHER", "slug": "X" })), json!(0));
        assert_eq!(firstPassFlag(&c, &json!({ "current_attempt": 1, "superseded_by": null, "slug": "X" })), json!(1));
        // violation by slug match
        assert_eq!(firstPassFlag(&c, &json!({ "current_attempt": 1, "superseded_by": null, "slug": "SLUG-2", "card_slug": "CARD-2" })), json!(0));
    }
}
