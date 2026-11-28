//! heimdall-classify.rs — twin of skills/heimdall/heimdall-classify.mjs (db READ lane, async-over-
//! sync, cross-db). NAMES ARE CONTRACT: `templateBase` (helper) · `heimdallClassify` (default). H0:
//! the router's first question — for substrate calls the category is a deterministic lookup keyed
//! by the template's BASE name (so version bumps keep their category); free-form text stays
//! 'general' honestly (flagged as such) until H6 wires the AI classifier.
// QUALITY: 84/100 — double-fallback lookup (mapped slug → general → hardcoded LANGUAGE/TALKER)

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (subset used).
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
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

fn version_suffix_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\.v\d+$").unwrap())
}

// String(name ?? '').replace(/\.v\d+$/, '')
pub fn templateBase(name: &Value) -> Value {
    let s = str_qq(Some(name));
    json!(version_suffix_re().replace(&s, "").into_owned())
}

const GENERAL: &str = "general";

fn cat_row(db: &Connection, slug: &str) -> Option<Value> {
    crate::db_json::query_rows(
        db,
        "SELECT slug, family, weight_profile FROM brain.heimdall_categories WHERE slug = ?",
        &[json!(slug)],
    )
    .ok()
    .and_then(|r| r.as_array().and_then(|a| a.first().cloned()))
}

pub fn heimdallClassify(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing('heimdall-classify','db')`
    // — the db lane always injects a live connection, so this guard is unreachable here.

    // `input?.template_name ? String(input.template_name) : null` — truthy-gated coercion, then
    // likewise for `text`.
    let template_name = if truthy(input.get("template_name")) { Some(str_qq(input.get("template_name"))) } else { None };
    let text = if truthy(input.get("text")) { Some(str_qq(input.get("text"))) } else { None };
    // JS throws err.required('heimdall-classify','template_name or text') when both are falsy —
    // a guard-throw, not exercised by fixtures (only success paths are, gotcha 9).
    if template_name.is_none() && text.is_none() {
        return Err("heimdall-classify: template_name or text is required".to_string());
    }

    if let Some(tn) = template_name {
        let base_v = templateBase(&json!(tn));
        let base = base_v.as_str().unwrap_or("").to_string();
        let map = crate::db_json::query_rows(
            db,
            "SELECT category FROM brain.heimdall_template_categories WHERE template_base = ?",
            &[json!(base.clone())],
        )
        .ok()
        .and_then(|r| r.as_array().and_then(|a| a.first().cloned()));
        // `map?.category ?? GENERAL`
        let slug = map.as_ref().and_then(|m| nz(m.get("category"))).cloned().unwrap_or_else(|| json!(GENERAL));
        let slug_str = slug.as_str().unwrap_or(GENERAL).to_string();
        // `catRow(slug) ?? catRow(GENERAL)`
        let cat = cat_row(db, &slug_str).or_else(|| cat_row(db, GENERAL));

        let mut out = Map::new();
        out.insert("category".into(), cat.as_ref().and_then(|c| nz(c.get("slug"))).cloned().unwrap_or_else(|| json!(GENERAL)));
        out.insert("family".into(), cat.as_ref().and_then(|c| nz(c.get("family"))).cloned().unwrap_or_else(|| json!("LANGUAGE")));
        out.insert("weight_profile".into(), cat.as_ref().and_then(|c| nz(c.get("weight_profile"))).cloned().unwrap_or_else(|| json!("TALKER")));
        out.insert("template_base".into(), json!(base));
        out.insert("source".into(), json!(if map.is_some() { "template-map" } else { "unmapped" }));
        out.insert("confidence".into(), json!(if map.is_some() { 1 } else { 0 }));
        return Ok(Value::Object(out));
    }

    // Free-form text: the AI path is H6. Until then: honest fallback.
    let cat = cat_row(db, GENERAL);
    let mut out = Map::new();
    out.insert("category".into(), json!(GENERAL));
    out.insert("family".into(), cat.as_ref().and_then(|c| nz(c.get("family"))).cloned().unwrap_or_else(|| json!("LANGUAGE")));
    out.insert("weight_profile".into(), cat.as_ref().and_then(|c| nz(c.get("weight_profile"))).cloned().unwrap_or_else(|| json!("TALKER")));
    out.insert("template_base".into(), Value::Null);
    out.insert("source".into(), json!("freeform-fallback"));
    out.insert("confidence".into(), json!(0));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_brain() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;
             CREATE TABLE brain.heimdall_categories (slug TEXT PRIMARY KEY, family TEXT NOT NULL, weight_profile TEXT NOT NULL);
             CREATE TABLE brain.heimdall_template_categories (template_base TEXT PRIMARY KEY, category TEXT NOT NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn template_base_strips_exactly_one_trailing_version() {
        assert_eq!(templateBase(&json!("blueprint.stub.v1")), json!("blueprint.stub"));
        assert_eq!(templateBase(&json!("interviewer.chat.v18")), json!("interviewer.chat"));
        assert_eq!(templateBase(&json!("no.version.suffix")), json!("no.version.suffix"));
        assert_eq!(templateBase(&Value::Null), json!(""));
    }

    #[test]
    fn known_template_resolves_via_base_name_version_bumps_keep_category() {
        let c = db_with_brain();
        c.execute("INSERT INTO brain.heimdall_template_categories VALUES ('blueprint.stub','code-stub')", []).unwrap();
        c.execute("INSERT INTO brain.heimdall_categories VALUES ('code-stub','BUILD','BUILDER')", []).unwrap();

        let r1 = heimdallClassify(&json!({ "template_name": "blueprint.stub.v1" }), &c).unwrap();
        assert_eq!(r1, json!({ "category": "code-stub", "family": "BUILD", "weight_profile": "BUILDER", "template_base": "blueprint.stub", "source": "template-map", "confidence": 1 }));

        // a future bump nobody registered still classifies — that is the point of base keys
        let r2 = heimdallClassify(&json!({ "template_name": "blueprint.stub.v9" }), &c).unwrap();
        assert_eq!(r2["category"], json!("code-stub"));
    }

    #[test]
    fn unknown_template_falls_back_to_general_unmapped() {
        let c = db_with_brain();
        c.execute("INSERT INTO brain.heimdall_categories VALUES ('general','LANGUAGE','TALKER')", []).unwrap();
        let r = heimdallClassify(&json!({ "template_name": "never.heard.of.it.v1" }), &c).unwrap();
        assert_eq!(r, json!({ "category": "general", "family": "LANGUAGE", "weight_profile": "TALKER", "template_base": "never.heard.of.it", "source": "unmapped", "confidence": 0 }));
    }

    #[test]
    fn mapped_but_dangling_category_double_falls_back_to_hardcoded_defaults() {
        let c = db_with_brain();
        // maps to a slug that has no row in heimdall_categories, and 'general' is absent too
        c.execute("INSERT INTO brain.heimdall_template_categories VALUES ('orphan','ghost-category')", []).unwrap();
        let r = heimdallClassify(&json!({ "template_name": "orphan.v2" }), &c).unwrap();
        assert_eq!(r, json!({ "category": "general", "family": "LANGUAGE", "weight_profile": "TALKER", "template_base": "orphan", "source": "template-map", "confidence": 1 }));
    }

    #[test]
    fn freeform_text_stays_general_until_h6() {
        let c = db_with_brain();
        c.execute("INSERT INTO brain.heimdall_categories VALUES ('general','LANGUAGE','TALKER')", []).unwrap();
        let r = heimdallClassify(&json!({ "text": "refactor my parser to be streaming" }), &c).unwrap();
        assert_eq!(r, json!({ "category": "general", "family": "LANGUAGE", "weight_profile": "TALKER", "template_base": Value::Null, "source": "freeform-fallback", "confidence": 0 }));
    }

    #[test]
    fn freeform_text_without_general_row_uses_hardcoded_defaults() {
        let c = db_with_brain();
        let r = heimdallClassify(&json!({ "text": "hello" }), &c).unwrap();
        assert_eq!(r, json!({ "category": "general", "family": "LANGUAGE", "weight_profile": "TALKER", "template_base": Value::Null, "source": "freeform-fallback", "confidence": 0 }));
    }
}
