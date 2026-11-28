//! test-smoke-gen.rs — twin of skills/codebase/test-smoke-gen.mjs (db READ lane). NAMES ARE CONTRACT:
//! `testSmokeGen` (default). Generate SMOKE Playwright specs from the UI surface (cross-module ui-surface-map).
// QUALITY: 79/100 — per-screen render + control-present asserts; deterministic codegen, ephemeral specs
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true }
}
fn str_qq(v: Option<&Value>) -> String {
    match v { None | Some(Value::Null) => String::new(), Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(), Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string() }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) } }

fn esc(s: &str) -> String { s.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', " ") }
fn slug(v: Option<&Value>) -> String {
    let s = match v { None | Some(Value::Null) => "screen".to_string(), Some(x) => str_qq(Some(x)) };
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"[^a-zA-Z0-9]+").unwrap());
    let out = re.replace_all(&s, "-").trim_matches('-').to_lowercase();
    if out.is_empty() { "screen".to_string() } else { out }
}

fn smoke_spec(screen: &Value, base_url: &str) -> String {
    let route_val = nz(screen.get("route")).cloned().unwrap_or(json!("/"));
    let route = str_qq(Some(&route_val));
    let url = format!("{}{}", base_url, route);
    let empty: Vec<Value> = vec![];
    let controls: Vec<&Value> = screen.get("controls").and_then(|v| v.as_array()).unwrap_or(&empty).iter().filter(|c| truthy(c.get("label"))).take(20).collect();
    let mut l: Vec<String> = Vec::new();
    l.push("import { test, expect } from '@playwright/test';".into());
    l.push(String::new());
    l.push(format!("// GENERATED smoke E2E — {} (route {}). Ephemeral: regenerated as the UI changes; not committed.", tmpl(screen.get("path")), route));
    l.push(format!("test.describe('smoke: {}', () => {{", esc(&route)));
    l.push("  test('renders without an uncaught error', async ({ page }) => {".into());
    l.push("    const errors: string[] = [];".into());
    l.push("    page.on('pageerror', (e) => errors.push(e.message));".into());
    l.push(format!("    await page.goto('{}');", esc(&url)));
    l.push("    await expect(page.locator('body')).toBeVisible();".into());
    l.push("    expect(errors, 'no uncaught errors on load').toEqual([]);".into());
    l.push("  });".into());
    for c in &controls {
        l.push(format!("  test('{}: {} is present', async ({{ page }}) => {{", esc(&str_qq(c.get("kind"))), esc(&str_qq(c.get("label")))));
        l.push(format!("    await page.goto('{}');", esc(&url)));
        l.push(format!("    await expect(page.getByText('{}', {{ exact: false }}).first()).toBeVisible({{ timeout: 5000 }});", esc(&str_qq(c.get("label")))));
        l.push("  });".into());
    }
    l.push("});".into());
    l.push(String::new());
    l.join("\n")
}

pub fn testSmokeGen(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let base_url = { let b = input.get("base_url").and_then(|v| v.as_str()).unwrap_or("http://localhost:5173"); b.strip_suffix('/').unwrap_or(b).to_string() };
    let map = crate::codebase_ui_surface_map::uiSurfaceMap(&json!({ "origin": origin }), db)?;
    let empty: Vec<Value> = vec![];
    let specs: Vec<Value> = map.get("screens").and_then(|v| v.as_array()).unwrap_or(&empty).iter().map(|s| {
        let route_or_path = nz(s.get("route")).or_else(|| nz(s.get("path"))).cloned().unwrap_or(Value::Null);
        let controls_n = s.get("controls").and_then(|v| v.as_array()).map_or(0, |a| a.iter().filter(|c| truthy(c.get("label"))).take(20).count());
        json!({ "name": format!("smoke-{}.spec.ts", slug(Some(&route_or_path))), "screen": route_or_path, "tests": 1 + controls_n, "code": smoke_spec(s, &base_url) })
    }).collect();
    let total_tests: i64 = specs.iter().map(|s| s.get("tests").and_then(|v| v.as_i64()).unwrap_or(0)).sum();
    Ok(json!({ "ok": true, "origin": origin, "base_url": base_url, "specs": specs, "summary": { "specs": specs.len(), "tests": total_tests, "committed": false } }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generates() {
        assert_eq!(slug(Some(&json!("/foo/bar"))), "foo-bar");
        assert_eq!(esc("a'b\\c\nd"), "a\\'b\\\\c d");
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_entry_points (id INTEGER PRIMARY KEY, origin TEXT, kind TEXT, subtype TEXT, path TEXT, label TEXT, line INTEGER, framework TEXT, http_method TEXT); \
          CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, direction TEXT, hops_json TEXT, linked_test TEXT); \
          INSERT INTO code_entry_points (origin,kind,subtype,path,label,line,framework) VALUES ('atlas','route','page-route','routes/foo.tsx','/foo',1,'react'),('atlas','ui','click','routes/foo.tsx','Submit',5,'react');").unwrap();
        let r = testSmokeGen(&json!({ "base_url": "http://localhost:5173" }), &c).unwrap();
        assert_eq!(r["specs"].as_array().unwrap().len(), 1);
        assert_eq!(r["specs"][0]["tests"], json!(2)); // render + 1 control
        assert_eq!(r["specs"][0]["name"], json!("smoke-foo.spec.ts"));
        assert!(r["specs"][0]["code"].as_str().unwrap().contains("page.goto('http://localhost:5173/foo')"));
        assert_eq!(r["summary"]["committed"], json!(false));
    }
}
