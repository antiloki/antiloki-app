//! design-audit.rs — twin of skills/codebase/design-audit.mjs (db READ lane). NAMES ARE CONTRACT:
//! `designAudit` (default). Deterministic design-token consistency audit (drift findings on-read).
// QUALITY: 82/100 — off-palette color, near-duplicate numeric clustering, no-scale magic-number detection
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

const NUMERIC: [&str; 6] = ["spacing", "radius", "font_size", "font_weight", "z", "opacity"];
const NEAR_CATS: [&str; 3] = ["spacing", "radius", "font_size"];
const NEAR_DELTA: f64 = 2.0;
const NEAR_REL: f64 = 0.2;
const NO_SCALE_MIN: usize = 6;

fn sev(s: &str) -> i64 { match s { "error" => 0, "warn" => 1, "info" => 2, _ => 3 } }
fn num_of(v: &Value) -> Option<f64> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"-?\d*\.?\d+").unwrap());
    let s = match v { Value::String(x) => x.clone(), Value::Number(n) => n.to_string(), Value::Null => "null".into(), o => o.to_string() };
    re.find(&s).and_then(|m| m.as_str().parse::<f64>().ok())
}
fn cat_of(r: &Value) -> Option<&str> { r.get("category").and_then(|v| v.as_str()) }
fn src_of(r: &Value) -> Vec<Value> {
    r.get("sources_json").and_then(|v| v.as_str()).and_then(|s| serde_json::from_str::<Value>(s).ok()).and_then(|v| v.as_array().cloned()).unwrap_or_default()
}
fn usage(r: &Value) -> i64 { r.get("usage_count").and_then(|v| v.as_i64()).unwrap_or(0) }

pub fn designAudit(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let rows = match crate::db_json::query_rows(db, "SELECT token, value, category, declared, usage_count, sources_json FROM code_design_tokens WHERE origin = ?", &[json!(origin)]) {
        Ok(r) => r.as_array().cloned().unwrap_or_default(),
        Err(_) => return Ok(json!({ "ok": true, "findings": [], "summary": { "tokens": 0, "note": "no code_design_tokens — rebuild the atlas first" } })),
    };
    let declared: Vec<&Value> = rows.iter().filter(|r| r.get("declared").map_or(false, |v| truthy(Some(v)))).collect();
    let literals: Vec<&Value> = rows.iter().filter(|r| !r.get("declared").map_or(false, |v| truthy(Some(v)))).collect();
    let declared_values: HashSet<String> = declared.iter().map(|r| str_qq(r.get("value"))).collect();
    let mut findings: Vec<Value> = Vec::new();

    // 1. off-palette colors
    if declared.iter().any(|r| cat_of(r) == Some("color")) {
        for r in literals.iter().filter(|r| cat_of(r) == Some("color") && !declared_values.contains(&str_qq(r.get("value")))) {
            findings.push(json!({ "kind": "off_palette_color", "category": "color", "severity": "warn",
                "title": format!("Hardcoded color \"{}\" — not a theme token", str_qq(r.get("value"))),
                "detail": "A color palette exists, but this value is hardcoded inline. Replace it with the nearest C.* token.",
                "values": [r.get("value").cloned().unwrap_or(Value::Null)], "usage_count": usage(r), "sources": src_of(r).into_iter().take(20).collect::<Vec<_>>() }));
        }
    }

    // 2. near-duplicate numeric clustering
    for cat in NEAR_CATS {
        let mut nums: Vec<(&&Value, f64)> = literals.iter().filter(|r| cat_of(r) == Some(cat)).filter_map(|r| num_of(r.get("value").unwrap_or(&Value::Null)).map(|n| (r, n))).collect();
        nums.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut i = 0;
        while i < nums.len() {
            let mut cluster: Vec<(&&Value, f64)> = vec![nums[i]];
            let mut j = i + 1;
            while j < nums.len() {
                let base = cluster[0].1; let prev = cluster.last().unwrap().1; let cur = nums[j].1;
                if cur - prev <= NEAR_DELTA && (cur - base) / cur.max(base).max(1.0) <= NEAR_REL { cluster.push(nums[j]); j += 1; } else { break; }
            }
            let mut vals: Vec<Value> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            for (r, _) in &cluster { let v = r.get("value").cloned().unwrap_or(Value::Null); if seen.insert(str_qq(Some(&v))) { vals.push(v); } }
            if cluster.len() >= 2 && vals.len() >= 2 {
                let vals_str: Vec<String> = vals.iter().map(|v| str_qq(Some(v))).collect();
                let total_usage: i64 = cluster.iter().map(|(r, _)| usage(r)).sum();
                let sources: Vec<Value> = cluster.iter().flat_map(|(r, _)| src_of(r)).take(20).collect();
                findings.push(json!({ "kind": "near_duplicate", "category": cat, "severity": "warn",
                    "title": format!("Near-duplicate {}: {} — consolidate to one token", cat, vals_str.join(", ")),
                    "detail": format!("{} almost-identical {} values. Palette rot — pick one and tokenize it.", cluster.len(), cat),
                    "values": vals, "usage_count": total_usage, "sources": sources }));
            }
            i = if j > i { j } else { i + 1 };
        }
    }

    // 3. no-scale
    for cat in NUMERIC {
        let lits: Vec<&&Value> = literals.iter().filter(|r| cat_of(r) == Some(cat)).collect();
        let mut distinct: Vec<Value> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for r in &lits { let v = r.get("value").cloned().unwrap_or(Value::Null); if seen.insert(str_qq(Some(&v))) { distinct.push(v); } }
        if !declared.iter().any(|r| cat_of(r) == Some(cat)) && distinct.len() >= NO_SCALE_MIN {
            let sites: i64 = lits.iter().map(|r| usage(r)).sum();
            let first5: Vec<String> = distinct.iter().take(5).map(|v| str_qq(Some(v))).collect();
            findings.push(json!({ "kind": "no_scale", "category": cat, "severity": "info",
                "title": format!("No {} scale — {} distinct hardcoded values across {} sites", cat, distinct.len(), sites),
                "detail": format!("No declared {} token scale; every value is a magic number. Introduce a scale (e.g. {}).", cat, first5.join(" / ")),
                "values": distinct.iter().take(12).cloned().collect::<Vec<_>>(), "usage_count": sites, "sources": [] }));
        }
    }

    findings.sort_by(|a, b| {
        let sa = sev(a.get("severity").and_then(|v| v.as_str()).unwrap_or(""));
        let sb = sev(b.get("severity").and_then(|v| v.as_str()).unwrap_or(""));
        sa.cmp(&sb).then_with(|| b.get("usage_count").and_then(|v| v.as_i64()).unwrap_or(0).cmp(&a.get("usage_count").and_then(|v| v.as_i64()).unwrap_or(0)))
    });
    let cnt = |k: &str| findings.iter().filter(|f| f.get("kind").and_then(|v| v.as_str()) == Some(k)).count();
    Ok(json!({ "ok": true, "findings": findings, "summary": {
        "tokens": rows.len(), "declared": declared.len(), "literals": literals.len(),
        "off_palette_colors": cnt("off_palette_color"), "near_duplicates": cnt("near_duplicate"), "categories_without_scale": cnt("no_scale") } }))
}

fn truthy(v: Option<&Value>) -> bool {
    match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true }
}
fn str_qq(v: Option<&Value>) -> String {
    match v { None | Some(Value::Null) => String::new(), Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(), Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audits() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_design_tokens (origin TEXT, token TEXT, value TEXT, category TEXT, declared INTEGER, usage_count INTEGER, sources_json TEXT); \
          INSERT INTO code_design_tokens VALUES \
            ('atlas','C.primary','#3366ff','color',1,1,'[]'),('atlas','lit1','#ff0000','color',0,3,'[]'), \
            ('atlas','s12','12px','spacing',0,2,'[]'),('atlas','s13','13px','spacing',0,4,'[]');").unwrap();
        let r = designAudit(&json!({ "origin": "atlas" }), &c).unwrap();
        let kinds: Vec<&str> = r["findings"].as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"off_palette_color")); // #ff0000 with a palette present
        assert!(kinds.contains(&"near_duplicate"));    // 12px vs 13px
        assert_eq!(r["summary"]["tokens"], json!(4));
    }
}
