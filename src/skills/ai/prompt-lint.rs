//! prompt-lint.rs — twin of skills/ai/prompt-lint.mjs (db READ lane, cross-db
//! brain.prompt_templates + brain.prompt_output_fields). NAMES ARE CONTRACT: `promptLint`
//! (default). Read-only scorecard over every ACTIVE prompt template (superseded_by IS NULL):
//! role_stated, has_output_contract, few_shot_examples, structural_coverage_pct, uses_system_md,
//! placeholder_count, has_cross_field_rules — weighted into score_0_100. Sorted worst-first
//! (score ASC, name tiebreak). WS-2 prompt-quality program (pre-approved plan 2026-06-10).
// QUALITY: pure scoring read; placeholder_count reuses ai_ai_call::extractPlaceholders (already a
// fixtured twin) instead of re-deriving the {{placeholder}} regex here.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (only the
// ones this module actually uses).
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')

// `/you are/i.test(...)`.
fn has_you_are(s: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)you are").unwrap()).is_match(s)
}

// `function countExampleMarkers(template_md)` — internal (not exported by the JS module either).
// Lines that read as a section marker mentioning examples. The leading marker group
// (#{1,6}|═+|\*\*|—|-{2,}) is itself optional and followed by an unrestricted `[^\n]*`, so in
// practice this counts any LINE containing the whole word "example"/"examples" (case-insensitive)
// — ported verbatim from the JS `/^[^\S\n]*(?:#{1,6}|═+|\*\*|—|-{2,})?[^\n]*\bexamples?\b[^\n]*$/gim`
// rather than algebraically simplified, to avoid a transcription mistake.
fn count_example_markers(template_md: &str) -> i64 {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?mi)^[^\S\n]*(?:#{1,6}|═+|\*\*|—|-{2,})?[^\n]*\bexamples?\b[^\n]*$").unwrap()
    });
    re.find_iter(template_md).count() as i64
}

// `f.min_length != null || f.max_length != null || (f.required_sections_json != null && ... !==
// '') || (f.pattern != null && ... !== '') || (f.enum_values != null && ... !== '')` — a string
// output field counts as structurally "constrained" when it declares length bounds, required
// sections, a pattern, or an enum.
fn is_constrained(f: &Value) -> bool {
    let present = |key: &str| !matches!(f.get(key), None | Some(Value::Null));
    let present_nonempty = |key: &str| match f.get(key) {
        None | Some(Value::Null) => false,
        Some(v) => v != &json!(""),
    };
    present("min_length") || present("max_length")
        || present_nonempty("required_sections_json")
        || present_nonempty("pattern")
        || present_nonempty("enum_values")
}

// `function scorePrompt(tpl, outputFields)` — internal (not exported). Scores one active
// template row (as returned by `SELECT * FROM brain.prompt_templates`) against its
// `brain.prompt_output_fields` rows.
fn score_prompt(tpl: &Value, output_fields: &[Value]) -> Value {
    let text = str_qq(tpl.get("template_md"));
    let system_md = tpl.get("system_md").and_then(Value::as_str).unwrap_or("").to_string();

    let role_stated = has_you_are(&text) || has_you_are(&system_md);
    let has_output_contract = !output_fields.is_empty();
    let few_shot_examples = count_example_markers(&text);

    let mut string_field_count: i64 = 0;
    let mut constrained_count: i64 = 0;
    for f in output_fields {
        if f.get("field_type").and_then(Value::as_str) == Some("string") {
            string_field_count += 1;
            if is_constrained(f) { constrained_count += 1; }
        }
    }
    let structural_coverage_pct: i64 = if string_field_count == 0 {
        100
    } else {
        ((constrained_count as f64 / string_field_count as f64) * 100.0).round() as i64
    };

    let uses_system_md = !system_md.trim().is_empty();
    let placeholders = crate::ai_ai_call::extractPlaceholders(&json!(text));
    let placeholder_count = placeholders.as_array().map(|a| a.len()).unwrap_or(0) as i64;

    let mut has_cross_field_rules = false;
    if truthy(tpl.get("cross_field_rules_json")) {
        if let Some(s) = tpl.get("cross_field_rules_json").and_then(Value::as_str) {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                has_cross_field_rules = parsed.as_array().map_or(false, |a| !a.is_empty());
            }
        }
        // malformed JSON → JS catch swallows it, has_cross_field_rules stays false; a non-string
        // truthy value never occurs for a TEXT column read via query_rows.
    }

    let raw_score = (if role_stated { 20.0 } else { 0.0 })
        + (if has_output_contract { 20.0 } else { 0.0 })
        + (if few_shot_examples > 0 { 15.0 } else { 0.0 })
        + (structural_coverage_pct as f64) * 0.25
        + (if uses_system_md { 10.0 } else { 0.0 })
        + (if has_cross_field_rules { 10.0 } else { 0.0 });
    let score_0_100 = raw_score.round() as i64;

    let mut checks = Map::new();
    checks.insert("role_stated".into(), json!(role_stated));
    checks.insert("has_output_contract".into(), json!(has_output_contract));
    checks.insert("few_shot_examples".into(), json!(few_shot_examples));
    checks.insert("structural_coverage_pct".into(), json!(structural_coverage_pct));
    checks.insert("uses_system_md".into(), json!(uses_system_md));
    checks.insert("placeholder_count".into(), json!(placeholder_count));
    checks.insert("has_cross_field_rules".into(), json!(has_cross_field_rules));

    let mut out = Map::new();
    out.insert("name".into(), tpl.get("name").cloned().unwrap_or(Value::Null));
    out.insert("version".into(), tpl.get("version").cloned().unwrap_or(Value::Null));
    out.insert("score_0_100".into(), json!(score_0_100));
    out.insert("checks".into(), Value::Object(checks));
    Value::Object(out)
}

pub fn promptLint(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing('prompt-lint', 'db')`
    // — unreachable here, db: &Connection is non-nullable.
    let name_opt = input.get("name");
    let templates_v = if truthy(name_opt) {
        let name_val = name_opt.cloned().unwrap_or(Value::Null);
        crate::db_json::query_rows(
            db,
            "SELECT * FROM brain.prompt_templates WHERE superseded_by IS NULL AND name = ?",
            &[name_val],
        )?
    } else {
        crate::db_json::query_rows(
            db,
            "SELECT * FROM brain.prompt_templates WHERE superseded_by IS NULL ORDER BY name",
            &[],
        )?
    };
    let templates = templates_v.as_array().cloned().unwrap_or_default();

    let mut prompts: Vec<Value> = Vec::with_capacity(templates.len());
    for tpl in &templates {
        let tpl_id = tpl.get("id").cloned().unwrap_or(Value::Null);
        let fields_v = crate::db_json::query_rows(
            db,
            "SELECT field_name, field_type, enum_values, min_length, max_length, \
             required_sections_json, pattern FROM brain.prompt_output_fields \
             WHERE prompt_template_id = ?",
            &[tpl_id],
        )?;
        let fields = fields_v.as_array().cloned().unwrap_or_default();
        prompts.push(score_prompt(tpl, &fields));
    }

    prompts.sort_by(|a, b| {
        let sa = a["score_0_100"].as_i64().unwrap_or(0);
        let sb = b["score_0_100"].as_i64().unwrap_or(0);
        sa.cmp(&sb).then_with(|| {
            let na = a["name"].as_str().unwrap_or("");
            let nb = b["name"].as_str().unwrap_or("");
            na.cmp(nb)
        })
    });

    Ok(json!({ "count": prompts.len() as i64, "prompts": prompts }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY AUTOINCREMENT, \
               name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, \
               system_md TEXT, cross_field_rules_json TEXT, superseded_by INTEGER); \
             CREATE TABLE brain.prompt_output_fields (id INTEGER PRIMARY KEY AUTOINCREMENT, \
               prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, \
               enum_values TEXT, min_length INTEGER, max_length INTEGER, \
               required_sections_json TEXT, pattern TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn rich_template_scores_all_checks_true() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id, name, version, template_md, system_md, cross_field_rules_json) VALUES \
             (1, 'lint.rich', 1, 'You are QA. Diagnose the failure for {{contract_slug}} using {{evidence}}.\n\n═══ EXAMPLE 1 — grounded suggestion ═══\n...\n═══ EXAMPLE 2 — escalation ═══\n...', \
              'You are the QA persona of the substrate.', '[{\"id\":\"r1\",\"expression\":\"true\",\"message\":\"always passes\"}]'); \
             INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type, max_length) VALUES (1, 'feedback_md', 'string', 2400); \
             INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type) VALUES (1, 'free_text', 'string'); \
             INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type) VALUES (1, 'items', 'array_of_string');",
        )
        .unwrap();
        let r = promptLint(&json!({ "name": "lint.rich" }), &c).unwrap();
        assert_eq!(r["count"], json!(1));
        let p = &r["prompts"][0];
        assert_eq!(p["name"], json!("lint.rich"));
        assert_eq!(p["version"], json!(1));
        assert_eq!(p["checks"]["role_stated"], json!(true));
        assert_eq!(p["checks"]["has_output_contract"], json!(true));
        assert_eq!(p["checks"]["few_shot_examples"], json!(2), "two EXAMPLE section markers");
        assert_eq!(p["checks"]["structural_coverage_pct"], json!(50));
        assert_eq!(p["checks"]["uses_system_md"], json!(true));
        assert_eq!(p["checks"]["placeholder_count"], json!(2));
        assert_eq!(p["checks"]["has_cross_field_rules"], json!(true));
        // 20 + 20 + 15 + 12.5 + 10 + 10 = 87.5 → 88
        assert_eq!(p["score_0_100"], json!(88));
    }

    #[test]
    fn bare_template_scores_low() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id, name, version, template_md) VALUES (1, 'lint.bare', 1, 'Summarize {{thing}} briefly.'); \
             INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type) VALUES (1, 'summary', 'string');",
        )
        .unwrap();
        let r = promptLint(&json!({ "name": "lint.bare" }), &c).unwrap();
        let p = &r["prompts"][0];
        assert_eq!(p["checks"]["role_stated"], json!(false));
        assert_eq!(p["checks"]["has_output_contract"], json!(true));
        assert_eq!(p["checks"]["few_shot_examples"], json!(0));
        assert_eq!(p["checks"]["structural_coverage_pct"], json!(0));
        assert_eq!(p["checks"]["uses_system_md"], json!(false));
        assert_eq!(p["checks"]["placeholder_count"], json!(1));
        assert_eq!(p["checks"]["has_cross_field_rules"], json!(false));
        assert_eq!(p["score_0_100"], json!(20));
    }

    #[test]
    fn superseded_version_is_excluded_both_filtered_and_unfiltered() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id, name, version, template_md, superseded_by) VALUES (1, 'lint.rich', 1, 'You are QA v1. {{a}}', 2); \
             INSERT INTO brain.prompt_templates (id, name, version, template_md) VALUES (2, 'lint.rich', 2, 'You are QA v2. {{a}}');",
        )
        .unwrap();
        let filtered = promptLint(&json!({ "name": "lint.rich" }), &c).unwrap();
        assert_eq!(filtered["count"], json!(1));
        assert_eq!(filtered["prompts"][0]["version"], json!(2));

        let unfiltered = promptLint(&json!({}), &c).unwrap();
        assert_eq!(unfiltered["count"], json!(1));
        assert_eq!(unfiltered["prompts"][0]["version"], json!(2));
    }

    #[test]
    fn no_filter_scans_all_active_sorted_worst_first_with_name_tiebreak() {
        let c = db();
        c.execute_batch(
            // Three templates that all resolve to the SAME score (no role claim, no output
            // fields, no system_md, no cross-field rules) — exercises the name tiebreak.
            "INSERT INTO brain.prompt_templates (id, name, version, template_md) VALUES (1, 'zzz.tie', 1, 'Plain filler text.'); \
             INSERT INTO brain.prompt_templates (id, name, version, template_md) VALUES (2, 'aaa.tie', 1, 'Plain filler text.'); \
             INSERT INTO brain.prompt_templates (id, name, version, template_md) VALUES (3, 'scan.rich', 1, 'You are helpful. {{x}}'); \
             INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type, pattern) VALUES (3, 'body', 'string', '^[A-Z]');",
        )
        .unwrap();
        let r = promptLint(&json!(null), &c).unwrap();
        let arr = r["prompts"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        for w in arr.windows(2) {
            assert!(w[0]["score_0_100"].as_i64().unwrap() <= w[1]["score_0_100"].as_i64().unwrap());
        }
        // the two tied templates (both structural_coverage_pct=100 via the empty-output-fields
        // branch, so score 25) come first, alphabetically ("aaa.tie" before "zzz.tie")
        assert_eq!(arr[0]["name"], json!("aaa.tie"));
        assert_eq!(arr[1]["name"], json!("zzz.tie"));
        assert_eq!(arr[2]["name"], json!("scan.rich"));
    }

    #[test]
    fn cross_field_rules_malformed_or_non_array_or_empty_all_count_as_absent() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id, name, version, template_md, cross_field_rules_json) VALUES (1, 'r.malformed', 1, 'x {{a}}', 'not valid json{'); \
             INSERT INTO brain.prompt_templates (id, name, version, template_md, cross_field_rules_json) VALUES (2, 'r.object', 1, 'x {{a}}', '{\"not\":\"an array\"}'); \
             INSERT INTO brain.prompt_templates (id, name, version, template_md, cross_field_rules_json) VALUES (3, 'r.empty', 1, 'x {{a}}', '[]'); \
             INSERT INTO brain.prompt_templates (id, name, version, template_md, cross_field_rules_json) VALUES (4, 'r.real', 1, 'x {{a}}', '[{\"id\":\"r1\"}]');",
        )
        .unwrap();
        let r = promptLint(&json!({}), &c).unwrap();
        let by_name = |n: &str| r["prompts"].as_array().unwrap().iter().find(|p| p["name"] == json!(n)).unwrap().clone();
        assert_eq!(by_name("r.malformed")["checks"]["has_cross_field_rules"], json!(false));
        assert_eq!(by_name("r.object")["checks"]["has_cross_field_rules"], json!(false));
        assert_eq!(by_name("r.empty")["checks"]["has_cross_field_rules"], json!(false));
        assert_eq!(by_name("r.real")["checks"]["has_cross_field_rules"], json!(true));
    }

    #[test]
    fn output_contract_and_structural_coverage_are_independent_of_string_subset() {
        let c = db();
        c.execute_batch(
            // Only a non-string field: has_output_contract is true (any field counts) but
            // structural_coverage_pct is the empty-string-fields default (100), not 0.
            "INSERT INTO brain.prompt_templates (id, name, version, template_md) VALUES (1, 'only.bool', 1, 'x {{a}}'); \
             INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type) VALUES (1, 'confirmed', 'boolean'); \
             INSERT INTO brain.prompt_templates (id, name, version, template_md) VALUES (2, 'empty.string.constraints', 1, 'x {{a}}'); \
             INSERT INTO brain.prompt_output_fields (prompt_template_id, field_name, field_type, enum_values, pattern, required_sections_json) VALUES (2, 'note', 'string', '', '', '');",
        )
        .unwrap();
        let r = promptLint(&json!({}), &c).unwrap();
        let by_name = |n: &str| r["prompts"].as_array().unwrap().iter().find(|p| p["name"] == json!(n)).unwrap().clone();
        let only_bool = by_name("only.bool");
        assert_eq!(only_bool["checks"]["has_output_contract"], json!(true));
        assert_eq!(only_bool["checks"]["structural_coverage_pct"], json!(100));
        let empty_constraints = by_name("empty.string.constraints");
        assert_eq!(empty_constraints["checks"]["has_output_contract"], json!(true));
        // empty-string constraint columns don't count — 0 of 1 string field constrained
        assert_eq!(empty_constraints["checks"]["structural_coverage_pct"], json!(0));
    }

    #[test]
    fn name_filter_matching_nothing_returns_empty() {
        let c = db();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id, name, version, template_md) VALUES (1, 'exists', 1, 'x');",
        )
        .unwrap();
        let r = promptLint(&json!({ "name": "does-not-exist" }), &c).unwrap();
        assert_eq!(r, json!({ "count": 0, "prompts": [] }));
    }
}
