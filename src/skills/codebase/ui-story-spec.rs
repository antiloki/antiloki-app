//! ui-story-spec.rs — twin of skills/codebase/ui-story-spec.mjs (db READ lane). NAMES ARE CONTRACT:
//! `uiStorySpec` (default) · `genSpec` (no-db helper). Recorded user story → deterministic Playwright E2E spec.
// QUALITY: 80/100 — steps→fill/click/submit, navigations→toHaveURL asserts; single-quote + regex escaping
use rusqlite::Connection;
use serde_json::{json, Value};

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
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
fn js_num_str(n: f64) -> String { if n.is_finite() && n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{}", n) } }
// sq: escape for a single-quoted JS string (backslash then quote)
fn sq(s: &str) -> String { s.replace('\\', "\\\\").replace('\'', "\\'") }
// reEsc: escape RegExp specials
fn re_esc(s: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"[.*+?^${}()|\[\]\\]").unwrap());
    re.replace_all(s, |c: &regex::Captures| format!("\\{}", &c[0])).into_owned()
}
// parse(s) = JSON.parse(s ?? 'null'); malformed → null
fn parse_json(v: Option<&Value>) -> Value {
    let text = match v { None | Some(Value::Null) => "null".to_string(), Some(Value::String(s)) => s.clone(), Some(o) => o.to_string() };
    serde_json::from_str(&text).unwrap_or(Value::Null)
}
fn jstr(v: &Value) -> String { serde_json::to_string(v).unwrap_or_else(|_| "null".into()) } // JSON.stringify

pub fn genSpec(story: &Value) -> Value {
    let name_val = if truthy(story.get("name")) { story.get("name").unwrap().clone() } else { json!("user story") };
    let url_str = if truthy(story.get("url")) { str_qq(story.get("url")) } else { "/".to_string() };
    let mut lines: Vec<String> = vec![
        "import { test, expect } from '@playwright/test';".to_string(),
        String::new(),
        format!("test({}, async ({{ page }}) => {{", jstr(&name_val)),
        format!("  await page.goto('{}');", sq(&url_str)),
    ];
    let steps = story.get("steps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for s in &steps {
        let action = s.get("action").and_then(|v| v.as_str());
        let sel = s.get("selector");
        if action == Some("type") && truthy(sel) {
            lines.push(format!("  await page.fill('{}', '{}');", sq(&str_qq(sel)), sq(&str_qq(s.get("value").filter(|v| !v.is_null())))));
        } else if action == Some("click") && truthy(sel) {
            lines.push(format!("  await page.click('{}');", sq(&str_qq(sel))));
        } else if action == Some("submit") && truthy(sel) {
            lines.push(format!("  // form submitted: '{}'", sq(&str_qq(sel))));
        } else if action == Some("navigate") && truthy(s.get("value")) {
            lines.push(format!("  await expect(page).toHaveURL(new RegExp({}));", jstr(&json!(re_esc(&str_qq(s.get("value")))))));
        }
    }
    lines.push("});".to_string());
    lines.push(String::new());
    json!(lines.join("\n"))
}

pub fn uiStorySpec(input: &Value, db: &Connection) -> Result<Value, String> {
    let idf = js_number(input.get("id"));
    if idf == 0.0 || idf.is_nan() { return Err("ui-story-spec: 'id' is required".into()); }
    let origin = input.get("origin").filter(|v| !v.is_null()).and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let rows = crate::db_json::query_rows(db, "SELECT name, url, steps_json FROM code_user_stories WHERE id = ? AND origin = ?", &[json!(idf as i64), json!(origin)])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let row = match rows.into_iter().next() { Some(r) => r, None => return Ok(json!({ "ok": false, "error": format!("story {} not found", js_num_str(idf)) })) };
    let steps = { let p = parse_json(row.get("steps_json")); if p.is_null() { json!([]) } else { p } };
    let story = json!({ "name": row.get("name").cloned().unwrap_or(Value::Null), "url": row.get("url").cloned().unwrap_or(Value::Null), "steps": steps });
    Ok(json!({ "ok": true, "name": row.get("name").cloned().unwrap_or(Value::Null), "spec": genSpec(&story) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generates() {
        let story = json!({ "name": "checkout", "url": "/cart", "steps": [
            { "action": "type", "selector": "#email", "value": "a@b.com" },
            { "action": "click", "selector": ".buy" },
            { "action": "navigate", "value": "/thanks?id=1" },
        ] });
        let spec = genSpec(&story).as_str().unwrap().to_string();
        assert!(spec.contains("test(\"checkout\", async ({ page }) => {"));
        assert!(spec.contains("await page.goto('/cart');"));
        assert!(spec.contains("await page.fill('#email', 'a@b.com');"));
        assert!(spec.contains("await page.click('.buy');"));
        assert!(spec.contains("toHaveURL(new RegExp(\"/thanks\\\\?id=1\"))"));
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_user_stories (id INTEGER, origin TEXT, name TEXT, url TEXT, steps_json TEXT); \
          INSERT INTO code_user_stories VALUES (1, 'atlas', 's1', '/x', '[{\"action\":\"click\",\"selector\":\"#go\"}]');").unwrap();
        let r = uiStorySpec(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["name"], json!("s1"));
        assert_eq!(uiStorySpec(&json!({ "id": 99 }), &c).unwrap(), json!({ "ok": false, "error": "story 99 not found" }));
    }
}
