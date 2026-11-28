//! categories.rs — twin of skills/constitution/_helpers/categories.mjs (no-db helper module). NAMES
//! ARE CONTRACT: `categoryOf` (named). Classify a law to one concern category (regex over its key).
// QUALITY: 80/100 — ordered regex/scope/detector rules, explicit-category-wins; regex crate for parity
use serde_json::{json, Value};
use std::sync::OnceLock;
use regex::Regex;

const LAW_CATEGORIES: [&str; 14] = [
    "security", "correctness", "architecture", "structure", "patterns", "testing",
    "framework", "naming", "style", "docs", "accessibility", "performance", "business", "project",
];

struct Res { sec: Regex, pat: Regex, a11y: Regex, test: Regex, testapp: Regex, arch: Regex, naming: Regex, perf: Regex, structure: Regex, docs: Regex, correct: Regex }
fn res() -> &'static Res {
    static C: OnceLock<Res> = OnceLock::new();
    C.get_or_init(|| Res {
        sec: Regex::new(r"secret|eval|inject|xss|\bsql\b|sanitiz|inner-?html|dangerously|document-write|href-javascript|new-function|insecure-http|no-exec|py-no-exec").unwrap(),
        pat: Regex::new(r"pattern|singleton|factory|observer|god-object|composition|inheritance").unwrap(),
        a11y: Regex::new(r"aria|a11y|semantic|keyboard|contrast|\balt\b").unwrap(),
        test: Regex::new(r"test|spec|\bonly\b|skip|coverage|assert").unwrap(),
        testapp: Regex::new(r"test|spec").unwrap(),
        arch: Regex::new(r"cycle|orphan|unreferenced|test-gap|god-file|layer").unwrap(),
        naming: Regex::new(r"naming|case|camel|pascal|snake|kebab").unwrap(),
        perf: Regex::new(r"loop|\bfor\b|while|memory|leak|listener|interval|subscription|perf|allocat|complexity").unwrap(),
        structure: Regex::new(r"max-file|max-fan|\bloc\b|nesting|param").unwrap(),
        docs: Regex::new(r"todo|fixme|comment|\bdoc").unwrap(),
        correct: Regex::new(r"swallow|catch|throw|equality|\bany\b|null|\bvar\b|debugger|console|ternary").unwrap(),
    })
}

pub fn categoryOf(law: &Value) -> Value {
    if let Some(cat) = law.get("category").and_then(|v| v.as_str()) {
        if LAW_CATEGORIES.contains(&cat) { return json!(cat); } // explicit wins
    }
    let k = law.get("key").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
    let scope = law.get("scope").and_then(|v| v.as_str());
    let det = law.get("detector").and_then(|v| v.as_str());
    let applies = law.get("applies_json").and_then(|v| v.as_str()).unwrap_or("");
    let r = res();
    let cat = if r.sec.is_match(&k) { "security" }
        else if r.pat.is_match(&k) { "patterns" }
        else if r.a11y.is_match(&k) { "accessibility" }
        else if r.test.is_match(&k) || r.testapp.is_match(applies) { "testing" }
        else if k.starts_with("arch-") || det == Some("layer") || scope == Some("architecture") || r.arch.is_match(&k) { "architecture" }
        else if r.naming.is_match(&k) { "naming" }
        else if r.perf.is_match(&k) { "performance" }
        else if det == Some("metric") || r.structure.is_match(&k) { "structure" }
        else if k.starts_with("react-") || k.starts_with("node-") || k.starts_with("py-") || scope == Some("framework") { "framework" }
        else if r.docs.is_match(&k) { "docs" }
        else if k == "lint-clean" { "style" }
        else if scope == Some("business") { "business" }
        else if scope == Some("project") { "project" }
        else if r.correct.is_match(&k) { "correctness" }
        else { "style" };
    json!(cat)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classifies() {
        assert_eq!(categoryOf(&json!({ "key": "no-eval" })), json!("security"));
        assert_eq!(categoryOf(&json!({ "key": "test-only" })), json!("testing"));
        assert_eq!(categoryOf(&json!({ "key": "react-hooks" })), json!("framework"));
        assert_eq!(categoryOf(&json!({ "key": "camelCase-fn" })), json!("naming"));
        assert_eq!(categoryOf(&json!({ "category": "docs", "key": "no-eval" })), json!("docs")); // explicit wins
        assert_eq!(categoryOf(&json!({ "key": "whatever-else" })), json!("style"));
    }
}
