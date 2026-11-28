//! design-style-guide.rs — twin of skills/codebase/design-style-guide.mjs (db READ/WRITE lane,
//! brain-attached for the prompt-template bootstrap). NAMES ARE CONTRACT: default `designStyleGuide`.
//! FEAT-DESIGN-SYSTEM P5 — the OPTIONAL AI designer: a LOW-tier narrative overview of the design
//! system, grounded STRICTLY in the deterministic facts from `designSystemGet` (already-converted
//! `codebase_design_system_get::designSystemGet` twin). This is a PRESENTATION layer only — it never
//! invents tokens or components, only narrates the facts it's handed.
//!
//! `context.db` missing → JS throws `err.contextMissing('design-style-guide', 'db')`. Not modeled:
//! `db: &Connection` is a required param here, so the null-db branch is structurally unreachable —
//! same convention as this crate's other codebase/ twins.
//!
//! PARITY-DEFERRED: the call ends in `crate::ai_ai_call::aiCall` (the deferred network-AI stub — see
//! ai-call.rs's module doc), which always `Err`s in this crate. Everything around it is ported
//! faithfully and IS the deterministic surface these tests exercise: the no-data early return, the
//! `ensure_prompt` bootstrap (reusing the already-converted `ai_prompt_find`/`ai_prompt_add` twins),
//! and the palette/components/findings summary-string assembly fed into `input_kv`. The `pick`
//! response-parse helper (mirrors JS's `output_kv.find(...)`) is ported and unit-tested against a
//! canned `aiCall`-shaped response value, though it is unreachable from `designStyleGuide` itself
//! since aiCall's Err short-circuits before any real response would be parsed.
use rusqlite::Connection;
use serde_json::{json, Value};

const PROMPT: &str = "design.styleguide.v1";
const SYSTEM: &str = "You are a precise design-systems writer. Given the DETERMINISTIC facts of a UI design system — its color/spacing/radius/font tokens, its component inventory, and its consistency-audit findings — write a SHORT style-guide overview. Ground every claim STRICTLY in the facts; never invent a token or component. Plain markdown, no code blocks.";
const TEMPLATE: &str = "Write a concise style-guide overview (markdown) for this design system, grounded ONLY in the facts below.\n\nPALETTE (by category): {{palette}}\nCOMPONENTS ({{component_count}}): {{components}}\nCONSISTENCY FINDINGS: {{findings}}\n\nSections: ## Palette (its character — how many colors, the spacing/radius/font scale or the lack of one); ## Components (the vocabulary — count, common props, composition); ## Consistency (the top drift to fix first). Put the whole overview in guide_md.";

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

// `if (existing && existing.template_md === TEMPLATE) return;` — else (re)write it.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    let up_to_date = existing
        .as_ref()
        .map(|e| e.get("template_md").and_then(|v| v.as_str()) == Some(TEMPLATE))
        .unwrap_or(false);
    if up_to_date {
        return Ok(());
    }
    let val = json!({
        "name": PROMPT,
        "template_md": TEMPLATE,
        "system_md": SYSTEM,
        "default_tier": "low",
        "default_temperature": "cold",
        "phase_flow": "single_only",
        "max_output_tokens": 900,
        "output_fields": [{ "field_name": "guide_md", "field_type": "string" }],
    });
    crate::ai_prompt_add::promptAdd(&val, db)?;
    Ok(())
}

// `res?.output_kv?.find((o) => o.field_name === 'guide_md' && o.parent_output_id == null)?.value_text ?? null`
fn pick(res: Option<&Value>) -> Option<Value> {
    let arr = res?.get("output_kv")?.as_array()?;
    arr.iter()
        .find(|o| {
            o.get("field_name").and_then(|v| v.as_str()) == Some("guide_md")
                && matches!(o.get("parent_output_id"), None | Some(Value::Null))
        })
        .and_then(|o| o.get("value_text").cloned())
}

pub fn designStyleGuide(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = match input.get("origin") {
        Some(Value::String(s)) => s.clone(),
        _ => "atlas".to_string(),
    };
    let data = crate::codebase_design_system_get::designSystemGet(&json!({ "origin": origin }), db)?;

    let tokens_n = data.get("summary").and_then(|s| s.get("tokens")).and_then(|v| v.as_i64()).unwrap_or(0);
    let components_n = data.get("summary").and_then(|s| s.get("components")).and_then(|v| v.as_i64()).unwrap_or(0);
    if tokens_n == 0 && components_n == 0 {
        return Ok(json!({ "ok": true, "guide_md": null, "generated": false, "note": "no design data — rebuild the atlas first" }));
    }
    ensure_prompt(db)?;

    let categories: Vec<String> = data.get("categories").and_then(|v| v.as_array()).cloned().unwrap_or_default()
        .iter().map(|c| str_qq(Some(c))).collect();
    let empty_palette = json!({ "declared": [], "literals": [] });
    let palette = categories.iter().map(|c| {
        let entry = data.get("palette").and_then(|p| p.get(c)).unwrap_or(&empty_palette);
        let declared = entry.get("declared").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let literals = entry.get("literals").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let names: Vec<String> = declared.iter().take(6).map(|t| {
            match t.get("token") {
                Some(Value::Null) | None => str_qq(t.get("value")),
                Some(tok) => str_qq(Some(tok)),
            }
        }).collect();
        format!("{}: {} declared ({}), {} literals", c, declared.len(), names.join(", "), literals.len())
    }).collect::<Vec<_>>().join("; ");

    let all_components = data.get("components").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let components = all_components.iter().take(40).map(|c| {
        let name = str_qq(c.get("name"));
        let props_n = c.get("props").and_then(|v| v.as_array()).map_or(0, |a| a.len());
        let renders = c.get("renders").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let renders_part = if !renders.is_empty() {
            format!(",→{}", renders.iter().take(3).map(|r| str_qq(Some(r))).collect::<Vec<_>>().join("/"))
        } else {
            String::new()
        };
        format!("{}({}p{})", name, props_n, renders_part)
    }).collect::<Vec<_>>().join(", ");

    let all_findings = data.get("audit").and_then(|a| a.get("findings")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let findings_str = all_findings.iter().take(12).map(|f| {
        format!("[{}] {}", str_qq(f.get("severity")), str_qq(f.get("title")))
    }).collect::<Vec<_>>().join("; ");
    let findings = if findings_str.is_empty() { "(none)".to_string() } else { findings_str };
    let palette_val = if palette.is_empty() { "(none)".to_string() } else { palette };
    let components_val = if components.is_empty() { "(none)".to_string() } else { components };
    let component_count = all_components.len().to_string();

    let call_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "palette", "value": palette_val },
            { "key": "component_count", "value": component_count },
            { "key": "components", "value": components_val },
            { "key": "findings", "value": findings },
        ],
        "tier": "low",
        "temperature": "cold",
        "phase_flow": "single_only",
    });
    let (guide_md, model_id) = match crate::ai_ai_call::aiCall(&call_input, db) {
        Ok(res) => {
            let gm = pick(Some(&res));
            let mid = res.get("model_id").cloned().unwrap_or(Value::Null);
            (gm, mid)
        }
        Err(_) => (None, Value::Null),
    };
    let generated = guide_md.as_ref().map(|v| truthy(Some(v)) && !str_qq(Some(v)).trim().is_empty()).unwrap_or(false);

    Ok(json!({ "ok": true, "guide_md": guide_md.unwrap_or(Value::Null), "model_id": model_id, "generated": generated }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(c: &Connection) {
        c.execute_batch(
            "CREATE TABLE code_design_tokens (origin TEXT, token TEXT, value TEXT, category TEXT, declared INTEGER, usage_count INTEGER, sources_json TEXT); \
             CREATE TABLE code_design_components (origin TEXT, name TEXT, path TEXT, line INTEGER, props_json TEXT, renders_json TEXT, tokens_json TEXT); \
             ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );"
        ).unwrap();
    }

    #[test]
    fn no_design_data_short_circuits_without_calling_ai() {
        let c = Connection::open_in_memory().unwrap();
        setup(&c);
        let r = designStyleGuide(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["guide_md"], Value::Null);
        assert_eq!(r["generated"], json!(false));
        assert_eq!(r["note"], json!("no design data — rebuild the atlas first"));
    }

    #[test]
    fn with_data_ensures_prompt_then_aicall_deferred_stub_errs_to_null_guide() {
        let c = Connection::open_in_memory().unwrap();
        setup(&c);
        c.execute(
            "INSERT INTO code_design_tokens VALUES ('atlas','C.primary','#fff','color',1,3,'[]')",
            [],
        ).unwrap();
        c.execute(
            "INSERT INTO code_design_components VALUES ('atlas','Button','b.tsx',5,'[\"onClick\"]','[]','[]')",
            [],
        ).unwrap();
        let r = designStyleGuide(&json!({ "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["guide_md"], Value::Null);
        assert_eq!(r["generated"], json!(false));
        assert!(r.get("model_id").is_some());
        // ensure_prompt actually wrote the row (bootstrap ran once, then is idempotent).
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'design.styleguide.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        designStyleGuide(&json!({ "origin": "atlas" }), &c).unwrap();
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'design.styleguide.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n2, 1, "up-to-date template_md skips rewrite");
    }

    #[test]
    fn pick_extracts_top_level_guide_md_ignoring_nested_parent_output_id() {
        let res = json!({ "model_id": "m1", "output_kv": [
            { "field_name": "guide_md", "parent_output_id": null, "value_text": "# Guide" },
            { "field_name": "guide_md", "parent_output_id": 3, "value_text": "nested" },
        ]});
        assert_eq!(pick(Some(&res)), Some(json!("# Guide")));
    }

    #[test]
    fn pick_returns_none_when_absent() {
        assert_eq!(pick(None), None);
        assert_eq!(pick(Some(&json!({}))), None);
    }
}
