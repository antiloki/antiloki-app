//! test-flow-gen.rs — twin of skills/codebase/test-flow-gen.mjs (db READ/WRITE lane,
//! cross-db `brain` for prompts). NAMES ARE CONTRACT: `testFlowGen` (default). FEAT-TEST-SCREEN
//! P3: generate FLOW Playwright specs from `code_data_stories` (AI-assisted) — where smoke (P2)
//! only claims "renders", each flow spec cites a data-story's user journey (hops) and asks a LOW
//! model to write MEANINGFUL assertions grounded in it. Ephemeral (generated, run live, not
//! committed). Best-effort: no provider/story → no spec, never an error.
//!
//! PARITY-DEFERRED: the per-story `await aiCall(...)` call is `crate::ai_ai_call::aiCall` (the
//! deferred network-AI stub — see ai-call.rs's module doc): it always `Err`s here, so `code` is
//! always `None`/`generated: false` in this twin's reachable surface — identical in shape to the
//! JS's own `catch { code = null }` fallback path. Everything AROUND the call is ported
//! faithfully and IS the deterministic surface these tests exercise: the origin/story_id guard +
//! query selection, the early-return "no data-stories" shape, prompt-template
//! ensure/bootstrap (`ensure_prompt`, reusing the already-converted `ai_prompt_find`/
//! `ai_prompt_add` twins), input_kv assembly (`hops_json` parse-or-empty, `slug`, base_url
//! trailing-slash strip), the `pick` output-field extractor (ported for completeness though
//! unreachable while aiCall always errors), and the final specs/summary shaping.
use rusqlite::Connection;
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String {
    match v { None | Some(Value::Null) => String::new(), Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(), Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string() }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

const PROMPT: &str = "test.flow.v1";
const SYSTEM: &str = "You write Playwright E2E tests from a described user JOURNEY. Given a data-story (the expected end-to-end flow + its hops), emit ONE Playwright test that drives the flow and makes MEANINGFUL assertions about the expected outcome — not just \"did not crash\". Ground every assertion in the story; prefer getByRole / getByText / getByTestId over brittle CSS. Output ONLY the spec code.";
const TEMPLATE: &str = "Write a Playwright flow test (real assertions) for this user journey.\n\nSTORY: {{name}}  (direction: {{direction}})\nDESCRIPTION: {{description}}\nHOPS (the path the data takes): {{hops}}\nBASE URL: {{base_url}}\n\nEmit a complete spec: the @playwright/test import + a test.describe + ONE test that walks the journey and\nasserts its expected outcome. Put the whole spec in spec_code.";

// = JS `String(s ?? 'flow').replace(/[^a-z0-9]+/gi, '-').replace(/^-+|-+$/g, '').toLowerCase() || 'flow'`
fn slug(v: Option<&Value>) -> String {
    let s = match v { None | Some(Value::Null) => "flow".to_string(), Some(x) => str_qq(Some(x)) };
    let mut out = String::new();
    let mut run = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            run = false;
        } else if !run {
            out.push('-');
            run = true;
        }
    }
    let out = out.trim_matches('-').to_lowercase();
    if out.is_empty() { "flow".to_string() } else { out }
}

// = JS `try { JSON.parse(s ?? '[]') } catch { [] }`
fn safe_parse(s: Option<&str>) -> Value {
    let raw = s.unwrap_or("[]");
    serde_json::from_str(raw).unwrap_or_else(|_| json!([]))
}

fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(row) = &existing {
        if nz(Some(row)).is_some() && row.get("template_md").and_then(|v| v.as_str()) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "low",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 1100,
            "output_fields": [{ "field_name": "spec_code", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// = JS `res?.output_kv?.find((o) => o.field_name === 'spec_code' && o.parent_output_id == null)?.value_text ?? null`
fn pick(res: Option<&Value>) -> Option<String> {
    let kv = res?.get("output_kv")?.as_array()?;
    kv.iter()
        .find(|o| o.get("field_name").and_then(|v| v.as_str()) == Some("spec_code") && nz(o.get("parent_output_id")).is_none())
        .and_then(|o| o.get("value_text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn testFlowGen(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let base_url = {
        let b = str_qq(nz(input.get("base_url")));
        let b = if b.is_empty() { "http://localhost:5173".to_string() } else { b };
        b.strip_suffix('/').map(|s| s.to_string()).unwrap_or(b)
    };

    let story_id = nz(input.get("story_id"));
    let stories: Vec<Value> = if let Some(id) = story_id {
        let mut stmt = db.prepare("SELECT id, name, description, direction, hops_json FROM code_data_stories WHERE id = ?").map_err(|e| e.to_string())?;
        let id_i = id.as_i64().unwrap_or(0);
        let row: Option<Value> = stmt
            .query_row([id_i], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, Option<String>>(1)?,
                    "description": r.get::<_, Option<String>>(2)?,
                    "direction": r.get::<_, Option<String>>(3)?,
                    "hops_json": r.get::<_, Option<String>>(4)?,
                }))
            })
            .ok();
        row.into_iter().collect()
    } else {
        let mut stmt = db.prepare("SELECT id, name, description, direction, hops_json FROM code_data_stories WHERE origin = ? ORDER BY id LIMIT 20").map_err(|e| e.to_string())?;
        let rows: Vec<Value> = stmt
            .query_map([&origin], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, Option<String>>(1)?,
                    "description": r.get::<_, Option<String>>(2)?,
                    "direction": r.get::<_, Option<String>>(3)?,
                    "hops_json": r.get::<_, Option<String>>(4)?,
                }))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    if stories.is_empty() {
        return Ok(json!({
            "ok": true, "origin": origin, "base_url": base_url, "specs": [],
            "summary": { "specs": 0, "generated": 0, "committed": false, "note": "no data-stories — author a journey first" }
        }));
    }

    ensure_prompt(db)?;
    let mut specs: Vec<Value> = Vec::new();
    for s in &stories {
        let name = str_qq(nz(s.get("name")));
        let name = if name.is_empty() { "(unnamed)".to_string() } else { name };
        let direction = { let d = str_qq(nz(s.get("direction"))); if d.is_empty() { "forward".to_string() } else { d } };
        let description = {
            let d = str_qq(nz(s.get("description")));
            let d = if d.is_empty() { "(no description)".to_string() } else { d };
            d.chars().take(1200).collect::<String>()
        };
        let hops_arr = safe_parse(s.get("hops_json").and_then(|v| v.as_str()));
        let hops: String = hops_arr
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|h| {
                        let label = nz(h.get("label")).or_else(|| nz(h.get("node_id")));
                        match label { Some(v) => str_qq(Some(v)), None => "?".to_string() }
                    })
                    .collect::<Vec<_>>()
                    .join(" → ")
            })
            .unwrap_or_default();
        let hops = if hops.is_empty() { "(no hops)".to_string() } else { hops };

        let input_kv = json!([
            { "key": "name", "value": name },
            { "key": "direction", "value": direction },
            { "key": "description", "value": description },
            { "key": "hops", "value": hops },
            { "key": "base_url", "value": base_url },
        ]);
        let ai_res = crate::ai_ai_call::aiCall(
            &json!({
                "prompt_name": PROMPT, "input_kv": input_kv,
                "tier": "low", "temperature": "cold", "phase_flow": "single_only",
            }),
            db,
        );
        let code = ai_res.ok().and_then(|r| pick(Some(&r)));

        let story_name = nz(s.get("name")).cloned().unwrap_or(Value::Null);
        specs.push(json!({
            "name": format!("flow-{}.spec.ts", slug(nz(s.get("name")))),
            "story": story_name,
            "story_id": s.get("id").cloned().unwrap_or(Value::Null),
            "code": code.clone(),
            "generated": code.as_ref().map_or(false, |c| !c.trim().is_empty()),
        }));
    }

    let generated = specs.iter().filter(|s| s.get("generated").and_then(|v| v.as_bool()).unwrap_or(false)).count();
    Ok(json!({
        "ok": true, "origin": origin, "base_url": base_url, "specs": specs,
        "summary": { "specs": specs.len(), "generated": generated, "committed": false }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(c: &Connection) {
        c.execute_batch(
            "ATTACH ':memory:' AS brain;
             CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, description TEXT, direction TEXT, hops_json TEXT, linked_test TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
    }

    #[test]
    fn slug_and_safe_parse() {
        assert_eq!(slug(Some(&json!("Search & Buy!"))), "search-buy");
        assert_eq!(slug(None), "flow");
        assert_eq!(safe_parse(Some("not json")), json!([]));
        assert_eq!(safe_parse(Some(r#"[{"label":"a"}]"#)), json!([{"label":"a"}]));
    }

    #[test]
    fn no_stories_returns_empty_without_touching_prompt() {
        let c = Connection::open_in_memory().unwrap();
        setup(&c);
        let r = testFlowGen(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["specs"], json!([]));
        assert_eq!(r["summary"]["note"], json!("no data-stories — author a journey first"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn story_generates_spec_shell_with_deferred_aicall() {
        let c = Connection::open_in_memory().unwrap();
        setup(&c);
        c.execute(
            "INSERT INTO code_data_stories (origin, name, description, direction, hops_json) VALUES ('atlas','Search Flow','user searches for a product','forward','[{\"label\":\"UI\"},{\"label\":\"API\"}]')",
            [],
        ).unwrap();
        let r = testFlowGen(&json!({ "origin": "atlas", "base_url": "http://localhost:5173/" }), &c).unwrap();
        assert_eq!(r["base_url"], json!("http://localhost:5173"));
        let specs = r["specs"].as_array().unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0]["name"], json!("flow-search-flow.spec.ts"));
        assert_eq!(specs[0]["story"], json!("Search Flow"));
        assert_eq!(specs[0]["code"], Value::Null);
        assert_eq!(specs[0]["generated"], json!(false));
        assert_eq!(r["summary"]["generated"], json!(0));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'test.flow.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn story_id_scoped_query() {
        let c = Connection::open_in_memory().unwrap();
        setup(&c);
        c.execute("INSERT INTO code_data_stories (origin, name, direction) VALUES ('atlas','A','forward')", []).unwrap();
        c.execute("INSERT INTO code_data_stories (origin, name, direction) VALUES ('atlas','B','forward')", []).unwrap();
        let r = testFlowGen(&json!({ "story_id": 2 }), &c).unwrap();
        let specs = r["specs"].as_array().unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0]["story"], json!("B"));
    }

    #[test]
    fn pick_extracts_top_level_field_only() {
        let res = json!({ "output_kv": [
            { "field_name": "spec_code", "parent_output_id": 1, "value_text": "nested" },
            { "field_name": "spec_code", "parent_output_id": null, "value_text": "top" },
        ]});
        assert_eq!(pick(Some(&res)), Some("top".to_string()));
        assert_eq!(pick(None), None);
    }
}
