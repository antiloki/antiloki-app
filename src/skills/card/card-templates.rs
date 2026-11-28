//! card-templates.rs — twin of skills/card/card-templates.mjs (no-db, pure data + helpers).
//! NAMES ARE CONTRACT: `toKebab`, `getTemplate`, `cardTemplates` (default). Built-in template
//! knowledge for card.draft.auto — deterministic fields the PL confirms/overrides, never blank.
//!
//! naming_from_intent is a FUNCTION VALUE per template in the JS source. The harness's
//! normalizeResult (orchestrator/compare.mjs) stringifies any function-typed property via
//! Function.prototype.toString() before comparing — so this twin embeds each arrow function's
//! EXACT source text (verbatim from the .mjs file) as a plain string in that slot. Not a
//! blocker: the harness never calls the function, it only compares its source text.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

fn kebab_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"[^a-z0-9]+").unwrap()) }

// JS String(v)
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

pub fn toKebab(s: &Value) -> Value {
    let lower = js_string(s).to_lowercase();
    let dashed = kebab_re().replace_all(&lower, "-");
    json!(dashed.trim_matches('-'))
}

#[allow(clippy::too_many_arguments)]
fn template(kind: &str, priority: i64, slug_prefix: &str, naming_src: &str, body_skeleton: &str, oos: &[&str], hints: &[&str]) -> Value {
    json!({
        "kind": kind,
        "default_priority": priority,
        "slug_prefix": slug_prefix,
        "naming_from_intent": naming_src,
        "body_skeleton": body_skeleton,
        "baseline_out_of_scope": oos,
        "baseline_acceptance_hints": hints,
    })
}

fn templates() -> Value {
    let mut m = Map::new();
    m.insert("bug-fix".into(), template("bug-fix", 2, "BUG",
        "(intent) => ({ slug_suggestion: `BUG-${toKebab(intent).slice(0, 40)}` })",
        "## Symptom\n<what the user observed>\n\n## Reproducer\n<exact steps to reproduce>\n\n## Root cause hypothesis\n<best current guess>\n\n## Acceptance\n- repro fails before fix\n- repro passes after fix\n- regression test committed",
        &["no refactoring of adjacent code", "no improving error messages elsewhere"],
        &["reproducer in test file", "regression test that captures the bug"]));
    m.insert("feature-add".into(), template("feature-add", 3, "FEAT",
        "(intent) => ({ slug_suggestion: `FEAT-${toKebab(intent).slice(0, 40)}` })",
        "## Why\n<user need this addresses>\n\n## Acceptance\n<what shipping means>\n\n## Out of scope\n<what NOT to include here>",
        &["no adjacent feature creep", "no schema migration unless required"],
        &["new action registered", "unit tests", "docs updated if user-visible"]));
    m.insert("refactor".into(), template("refactor", 4, "REFACTOR",
        "(intent) => ({ slug_suggestion: `REFACTOR-${toKebab(intent).slice(0, 40)}` })",
        "## What changes shape\n<the structural change>\n\n## Why now\n<the trigger>\n\n## Behavior invariant\n<what MUST stay observably identical>",
        &["no behavior changes", "no new features piggybacking"],
        &["existing tests pass unchanged", "no new failures", "public API identical"]));
    m.insert("tech-debt".into(), template("tech-debt", 4, "TECHDEBT",
        "(intent) => ({ slug_suggestion: `TECHDEBT-${toKebab(intent).slice(0, 40)}` })",
        "## The debt\n<what is owed>\n\n## Cost of paying\n<effort estimate>\n\n## Cost of not paying\n<what breaks if we wait>",
        &["no scope expansion", "no opportunistic improvements"],
        &["identifiable cleanup", "tests still pass"]));
    m.insert("arch".into(), template("arch", 2, "ARCH",
        "(intent) => ({ slug_suggestion: `ARCH-${toKebab(intent).slice(0, 40)}` })",
        "## Decision\n<the architectural choice>\n\n## Alternatives considered\n<what we rejected and why>\n\n## Consequences\n<good + bad downstream effects>",
        &["no implementation in this card — decision only", "follow-up cards file impl work"],
        &["decision documented", "rationale recorded in laws.sqlite if hardening"]));
    m.insert("docs".into(), template("docs", 4, "DOCS",
        "(intent) => ({ slug_suggestion: `DOCS-${toKebab(intent).slice(0, 40)}` })",
        "## What\n<the docs change>\n\n## Audience\n<who reads this>\n\n## Acceptance\n<rendered output verified>",
        &["no behavioral changes", "no script additions"],
        &["rendered HTML/MD reviewed", "cross-links work"]));
    m.insert("ops".into(), template("ops", 3, "OPS",
        "(intent) => ({ slug_suggestion: `OPS-${toKebab(intent).slice(0, 40)}` })",
        "## What\n<the operational change>\n\n## Risk\n<what could go sideways>\n\n## Rollback\n<how we undo if needed>",
        &["no code changes — config/infra only"],
        &["pre-commit gate green", "consistency-scan clean"]));
    Value::Object(m)
}

fn kinds() -> Value { json!(["bug-fix", "feature-add", "refactor", "tech-debt", "arch", "docs", "ops"]) }

pub fn getTemplate(kind: &Value) -> Value {
    let key = js_string(kind);
    templates().get(&key).cloned().unwrap_or(Value::Null)
}

pub fn cardTemplates(_input: &Value, _db: &Connection) -> Result<Value, String> {
    Ok(json!({ "kinds": kinds(), "templates": templates() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_collapses_and_trims() {
        assert_eq!(toKebab(&json!("  Hello, World!!  ")), json!("hello-world"));
        assert_eq!(toKebab(&json!("___leading and trailing___")), json!("leading-and-trailing"));
        assert_eq!(toKebab(&json!("Already-Kebab-Case")), json!("already-kebab-case"));
        assert_eq!(toKebab(&Value::Null), json!("null"));
        assert_eq!(toKebab(&json!("")), json!(""));
        // JS /[^a-z0-9]+/g is ASCII-only: the accented chars (Ü î ç ø é) each fail [a-z0-9] and
        // collapse into runs with adjacent non-matching chars, but the plain ASCII 'n' between
        // Ü and î does NOT match, breaking the run in two — "ü" collapses alone, then "n" stays
        // literal, then "îçøé " (through the trailing space) collapses as one run before "mix".
        assert_eq!(toKebab(&json!("\u{dc}n\u{ee}\u{e7}\u{f8}\u{e9} mix 123")), json!("n-mix-123"));
    }

    #[test]
    fn get_template_known_and_unknown() {
        let bf = getTemplate(&json!("bug-fix"));
        assert_eq!(bf["kind"], json!("bug-fix"));
        assert_eq!(bf["default_priority"], json!(2));
        assert_eq!(bf["slug_prefix"], json!("BUG"));
        assert_eq!(bf["naming_from_intent"], json!("(intent) => ({ slug_suggestion: `BUG-${toKebab(intent).slice(0, 40)}` })"));
        assert_eq!(getTemplate(&json!("ops"))["slug_prefix"], json!("OPS"));
        assert_eq!(getTemplate(&json!("not-a-real-kind")), Value::Null);
        assert_eq!(getTemplate(&Value::Null), Value::Null);
    }

    #[test]
    fn card_templates_lists_kinds_and_templates() {
        let conn = Connection::open_in_memory().unwrap();
        let r = cardTemplates(&json!({}), &conn).unwrap();
        assert_eq!(r["kinds"], json!(["bug-fix", "feature-add", "refactor", "tech-debt", "arch", "docs", "ops"]));
        let tmpl = &r["templates"];
        assert_eq!(tmpl.as_object().unwrap().len(), 7);
        assert_eq!(tmpl["arch"]["baseline_out_of_scope"], json!(["no implementation in this card \u{2014} decision only", "follow-up cards file impl work"]));
        // ignores input/context entirely — same output regardless
        assert_eq!(cardTemplates(&Value::Null, &conn).unwrap(), r);
    }
}
