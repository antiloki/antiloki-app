//! heimdall-validate.rs — twin of skills/heimdall/heimdall-validate.mjs (db READ lane, async-over-
//! sync, cross-db). NAMES ARE CONTRACT: `heimdallValidate` (default). H0 invariant checks (docs/
//! heimdall-plan.md §7.4): every weight profile's weights_json parses to an object of finite
//! numbers; every category references an existing weight profile; every template→category mapping
//! points at an existing category; every DISTINCT live prompt_templates.name (by BASE) is mapped —
//! unmapped live templates are the failure this skill exists to catch; every mapped category is
//! 'active'. Deterministic, read-only — the H0 exit gate is this being green.
// QUALITY: 82/100 — five independent invariant scans over the seeded heimdall tables, folded into one findings[]

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// Object.entries(w ?? {}) for a value coming from JSON.parse (never undefined). Object/Array give
// the expected key/value pairs (array indices become string keys); a bare string boxes to indexed
// chars (BMP-only — fixtures are ASCII); number/bool/null box to a wrapper with no own enumerable
// properties -> [] (this also covers the `?? {}` null case, since Object.entries({}) is []).
fn js_object_entries(v: &Value) -> Vec<(String, Value)> {
    match v {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Value::Array(a) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v.clone())).collect(),
        Value::String(s) => s.chars().enumerate().map(|(i, c)| (i.to_string(), json!(c.to_string()))).collect(),
        _ => Vec::new(),
    }
}

fn finding(check: &str, severity: &str, detail: String) -> Value {
    json!({ "check": check, "severity": severity, "detail": detail })
}

pub fn heimdallValidate(_input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing('heimdall-validate',
    // 'db')` — the db lane always injects a live connection, so this guard is unreachable here.
    // `input` is unused: the JS doc's checks read only from context.db.
    let mut findings: Vec<Value> = Vec::new();
    let mut checks: i64 = 0;

    // 1. profiles parse
    checks += 1;
    let profiles = crate::db_json::query_rows(db, "SELECT slug, weights_json FROM brain.heimdall_weight_profiles", &[])
        .ok()
        .and_then(|r| r.as_array().cloned())
        .unwrap_or_default();
    let profile_slugs: HashSet<String> = profiles.iter().map(|p| str_qq(p.get("slug"))).collect();
    for p in &profiles {
        let slug = str_qq(p.get("slug"));
        let raw = str_qq(p.get("weights_json"));
        match serde_json::from_str::<Value>(&raw) {
            Ok(w) => {
                let entries = js_object_entries(&w);
                let bad: Vec<String> = entries.iter().filter(|(_, v)| !matches!(v, Value::Number(_))).map(|(k, _)| k.clone()).collect();
                let falsy = !truthy(Some(&w));
                let not_object_type = !matches!(w, Value::Object(_) | Value::Array(_) | Value::Null);
                if falsy || not_object_type || !bad.is_empty() {
                    let list = if bad.is_empty() { "(not an object)".to_string() } else { bad.join(", ") };
                    findings.push(finding("profiles", "error", format!("profile '{slug}' weights_json has non-numeric entries: {list}")));
                }
            }
            Err(_) => {
                findings.push(finding("profiles", "error", format!("profile '{slug}' weights_json does not parse")));
            }
        }
    }
    if profiles.is_empty() {
        findings.push(finding("profiles", "error", "no weight profiles seeded".to_string()));
    }

    // 2. categories reference existing profiles
    checks += 1;
    let cats = crate::db_json::query_rows(db, "SELECT slug, weight_profile, status FROM brain.heimdall_categories", &[])
        .ok()
        .and_then(|r| r.as_array().cloned())
        .unwrap_or_default();
    let cat_slugs: HashSet<String> = cats.iter().map(|c| str_qq(c.get("slug"))).collect();
    for c in &cats {
        let weight_profile = str_qq(c.get("weight_profile"));
        if !profile_slugs.contains(&weight_profile) {
            findings.push(finding(
                "categories",
                "error",
                format!("category '{}' references missing weight profile '{}'", str_qq(c.get("slug")), weight_profile),
            ));
        }
    }
    if cats.is_empty() {
        findings.push(finding("categories", "error", "no categories seeded".to_string()));
    }

    // 3. mappings point at existing categories
    checks += 1;
    let maps = crate::db_json::query_rows(db, "SELECT template_base, category FROM brain.heimdall_template_categories", &[])
        .ok()
        .and_then(|r| r.as_array().cloned())
        .unwrap_or_default();
    for m in &maps {
        let category = str_qq(m.get("category"));
        if !cat_slugs.contains(&category) {
            findings.push(finding(
                "mappings",
                "error",
                format!("mapping '{}' \u{2192} missing category '{}'", str_qq(m.get("template_base")), category),
            ));
        }
    }

    // 4. every live template (by base) is mapped
    checks += 1;
    let mapped_bases: HashSet<String> = maps.iter().map(|m| str_qq(m.get("template_base"))).collect();
    // `try { ... } catch { /* prompt_templates absent on a bare DB */ }` — query_rows' Err (e.g.
    // "no such table") is swallowed via .ok(), leaving liveNames empty exactly like the JS catch.
    let live_names: Vec<String> = crate::db_json::query_rows(db, "SELECT DISTINCT name FROM brain.prompt_templates", &[])
        .ok()
        .and_then(|r| r.as_array().cloned())
        .map(|rows| rows.iter().map(|r| str_qq(r.get("name"))).collect())
        .unwrap_or_default();
    // `[...new Set(liveNames.map(templateBase))]` — dedupe bases, keep first-occurrence order.
    let mut seen_bases: HashSet<String> = HashSet::new();
    let mut unique_bases: Vec<String> = Vec::new();
    for n in &live_names {
        let base = crate::heimdall_heimdall_classify::templateBase(&json!(n)).as_str().unwrap_or("").to_string();
        if seen_bases.insert(base.clone()) {
            unique_bases.push(base);
        }
    }
    let unmapped: Vec<&String> = unique_bases.iter().filter(|b| !b.is_empty() && !mapped_bases.contains(*b)).collect();
    for b in &unmapped {
        findings.push(finding(
            "live-templates",
            "error",
            format!("live template base '{b}' has no category mapping \u{2014} add it to canonical-heimdall.mjs"),
        ));
    }

    // 5. mapped categories are active
    checks += 1;
    let active_slugs: HashSet<String> = cats
        .iter()
        .filter(|c| c.get("status").and_then(|v| v.as_str()) == Some("active"))
        .map(|c| str_qq(c.get("slug")))
        .collect();
    let mut seen_map_cats: HashSet<String> = HashSet::new();
    for m in &maps {
        let slug = str_qq(m.get("category"));
        if seen_map_cats.insert(slug.clone()) && cat_slugs.contains(&slug) && !active_slugs.contains(&slug) {
            findings.push(finding(
                "status",
                "warn",
                format!("category '{slug}' is mapped but still 'dormant' \u{2014} seed status refresh did not run"),
            ));
        }
    }

    let ok = !findings.iter().any(|f| f.get("severity").and_then(|v| v.as_str()) == Some("error"));

    let mut out = Map::new();
    out.insert("ok".into(), json!(ok));
    out.insert("checks".into(), json!(checks));
    out.insert("findings".into(), Value::Array(findings));
    out.insert(
        "counts".into(),
        json!({
            "profiles": profiles.len() as i64,
            "categories": cats.len() as i64,
            "mappings": maps.len() as i64,
            "live_template_bases": unique_bases.len() as i64,
        }),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;
             CREATE TABLE brain.heimdall_weight_profiles (slug TEXT PRIMARY KEY, weights_json TEXT NOT NULL);
             CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, weight_profile TEXT NOT NULL, status TEXT NOT NULL);
             CREATE TABLE brain.heimdall_template_categories (template_base TEXT PRIMARY KEY, category TEXT NOT NULL);
             CREATE TABLE brain.prompt_templates (name TEXT NOT NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn green_on_a_freshly_seeded_db() {
        let c = fresh();
        c.execute("INSERT INTO brain.heimdall_weight_profiles VALUES ('TALKER', '{\"cognition\":1,\"structure\":1}')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_categories VALUES ('code-stub','TALKER','active')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_template_categories VALUES ('blueprint.stub','code-stub')", []).unwrap();
        c.execute("INSERT INTO brain.prompt_templates VALUES ('blueprint.stub.v1')", []).unwrap();
        let r = heimdallValidate(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["findings"], json!([]));
        assert_eq!(r["checks"], json!(5));
        assert_eq!(r["counts"], json!({ "profiles": 1, "categories": 1, "mappings": 1, "live_template_bases": 1 }));
    }

    #[test]
    fn empty_db_reports_no_profiles_and_no_categories() {
        let c = fresh();
        let r = heimdallValidate(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let checks: HashSet<String> = r["findings"].as_array().unwrap().iter().map(|f| f["check"].as_str().unwrap().to_string()).collect();
        assert!(checks.contains("profiles"));
        assert!(checks.contains("categories"));
        assert_eq!(r["counts"], json!({ "profiles": 0, "categories": 0, "mappings": 0, "live_template_bases": 0 }));
    }

    #[test]
    fn a_live_template_with_no_mapping_is_an_error_naming_the_base() {
        let c = fresh();
        c.execute("INSERT INTO brain.prompt_templates VALUES ('rogue.new.stage.v1')", []).unwrap();
        let r = heimdallValidate(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let f = r["findings"].as_array().unwrap().iter().find(|f| f["check"] == json!("live-templates")).unwrap();
        assert!(f["detail"].as_str().unwrap().contains("rogue.new.stage"));
    }

    #[test]
    fn version_bumps_collapse_to_one_base_and_stay_mapped() {
        let c = fresh();
        c.execute("INSERT INTO brain.heimdall_weight_profiles VALUES ('TALKER', '{\"a\":1}')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_categories VALUES ('code-stub','TALKER','active')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_template_categories VALUES ('blueprint.stub','code-stub')", []).unwrap();
        c.execute("INSERT INTO brain.prompt_templates VALUES ('blueprint.stub.v1')", []).unwrap();
        c.execute("INSERT INTO brain.prompt_templates VALUES ('blueprint.stub.v9')", []).unwrap();
        let r = heimdallValidate(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["counts"]["live_template_bases"], json!(1));
    }

    #[test]
    fn mapping_to_a_missing_category_is_an_error() {
        let c = fresh();
        c.execute("INSERT INTO brain.heimdall_template_categories VALUES ('x.y', 'no-such-category')", []).unwrap();
        let r = heimdallValidate(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let f = r["findings"].as_array().unwrap().iter().find(|f| f["check"] == json!("mappings")).unwrap();
        assert!(f["detail"].as_str().unwrap().contains("no-such-category"));
        assert!(f["detail"].as_str().unwrap().contains("\u{2192}"));
    }

    #[test]
    fn unparseable_profile_weights_is_an_error() {
        let c = fresh();
        c.execute("INSERT INTO brain.heimdall_weight_profiles VALUES ('TALKER', '{broken')", []).unwrap();
        let r = heimdallValidate(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let f = r["findings"].as_array().unwrap().iter().find(|f| f["check"] == json!("profiles")).unwrap();
        assert!(f["detail"].as_str().unwrap().contains("does not parse"));
    }

    #[test]
    fn non_numeric_weight_entries_are_listed_by_key() {
        let c = fresh();
        c.execute("INSERT INTO brain.heimdall_weight_profiles VALUES ('BUILDER', '{\"cognition\":1,\"note\":\"x\"}')", []).unwrap();
        let r = heimdallValidate(&json!({}), &c).unwrap();
        let f = r["findings"].as_array().unwrap().iter().find(|f| f["check"] == json!("profiles")).unwrap();
        assert!(f["detail"].as_str().unwrap().ends_with("note"));
    }

    #[test]
    fn non_object_weights_json_reports_not_an_object() {
        let c = fresh();
        c.execute("INSERT INTO brain.heimdall_weight_profiles VALUES ('TALKER', '5')", []).unwrap();
        let r = heimdallValidate(&json!({}), &c).unwrap();
        let f = r["findings"].as_array().unwrap().iter().find(|f| f["check"] == json!("profiles")).unwrap();
        assert!(f["detail"].as_str().unwrap().ends_with("(not an object)"));
    }

    #[test]
    fn dormant_mapped_category_is_a_warning_not_an_error() {
        let c = fresh();
        c.execute("INSERT INTO brain.heimdall_weight_profiles VALUES ('BUILDER', '{\"a\":1}')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_categories VALUES ('conversational','BUILDER','dormant')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_template_categories VALUES ('chat','conversational')", []).unwrap();
        let r = heimdallValidate(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true)); // warn only, no error
        let f = r["findings"].as_array().unwrap().iter().find(|f| f["check"] == json!("status")).unwrap();
        assert_eq!(f["severity"], json!("warn"));
        assert!(f["detail"].as_str().unwrap().contains("'conversational'"));
        assert!(f["detail"].as_str().unwrap().contains("dormant"));
    }
}
