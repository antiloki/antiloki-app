//! project-rules.rs — the project's OWN house rules, read from its lint config so a seed law
//! does not overrule a decision the codebase already made and enforces on itself.
//!
//! NO JS TWIN. Added 2026-09-12 after the folder-paper audit: `no-loose-equality` flagged four
//! `!= null` checks in a repository whose `.eslintrc.js` says `eqeqeq: ['error', 'allow-null']`
//! — the project permits exactly that idiom, and the paper's "single best move" was to undo it.
//!
//! Scope, stated plainly: this reads ONE rule at a time, by name, and returns its level and
//! option in a normalised shape. It is not an ESLint config resolver — no `extends` chains, no
//! plugin defaults, no overrides blocks. JSON configs are parsed properly; JS/CJS/MJS/flat
//! configs are scanned with a regex for the literal rule entry, which catches the way rules are
//! written in practice (`eqeqeq: ['error', 'allow-null']`) and honestly returns `null` for
//! anything computed. A `null` means "the project did not say", never "the project said off".
use serde_json::{json, Value};
use std::path::Path;

const JSON_CANDIDATES: &[&str] = &[".eslintrc.json", ".eslintrc"];
const JS_CANDIDATES: &[&str] = &[".eslintrc.js", ".eslintrc.cjs", ".eslintrc.mjs", "eslint.config.js", "eslint.config.mjs", "eslint.config.cjs", "eslint.config.ts"];

/// ESLint's level vocabulary, normalised: `0|"off"` → off, `1|"warn"` → warn, `2|"error"` → error.
fn level_of(v: &Value) -> Option<&'static str> {
    match v {
        Value::Number(n) => match n.as_i64() {
            Some(0) => Some("off"),
            Some(1) => Some("warn"),
            Some(2) => Some("error"),
            _ => None,
        },
        Value::String(s) => match s.as_str() {
            "off" => Some("off"),
            "warn" => Some("warn"),
            "error" => Some("error"),
            _ => None,
        },
        _ => None,
    }
}

/// `"error"` / `["error", "allow-null"]` / `["error", {"null": "ignore"}]` → `{level, option}`.
fn normalise(setting: &Value) -> Value {
    let (level_v, option_v) = match setting {
        Value::Array(a) => (a.first().cloned().unwrap_or(Value::Null), a.get(1).cloned().unwrap_or(Value::Null)),
        other => (other.clone(), Value::Null),
    };
    let Some(level) = level_of(&level_v) else { return Value::Null };
    // eqeqeq's second slot: a string ("always" | "smart" | "allow-null") or an options object
    // ({"null": "ignore" | "always" | "never"}). Both spell the same three intents.
    let option = match &option_v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("null").and_then(Value::as_str).map(|n| match n {
            "ignore" => "allow-null".to_string(),
            "never" => "never-null".to_string(),
            _ => "always".to_string(),
        }),
        _ => None,
    };
    json!({ "level": level, "option": option })
}

fn from_json_config(text: &str, rule: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(text).ok()?;
    let setting = v.get("rules")?.get(rule)?;
    Some(normalise(setting))
}

/// Best-effort read of a rule out of a JS-shaped config: the literal entry
/// `eqeqeq: ['error', 'allow-null']` / `"eqeqeq": "off"` / `eqeqeq: [ERROR, 'allow-null']`
/// (React's config uses named constants for levels — resolved for the three ESLint names).
fn from_js_config(text: &str, rule: &str) -> Option<Value> {
    let re = regex::Regex::new(&format!(r#"['"]?{}['"]?\s*:\s*(\[[^\]]*\]|['"][A-Za-z-]+['"]|\d|[A-Z_]+)"#, regex::escape(rule))).ok()?;
    let m = re.captures(text)?;
    let raw = m.get(1)?.as_str().trim();
    // turn the JS literal into JSON: quotes → double, bare level constants → their names
    let jsonish = raw
        .replace('\'', "\"")
        .replace("ERROR", "\"error\"")
        .replace("WARN", "\"warn\"")
        .replace("OFF", "\"off\"");
    let v: Value = serde_json::from_str(&jsonish).ok()?;
    Some(normalise(&v))
}

/// readEslintRule(root, rule) → `{level: "error"|"warn"|"off", option: string|null}` or Null
/// when the project's config does not mention the rule (or has no config).
#[allow(non_snake_case)]
pub fn readEslintRule(root: &str, rule: &str) -> Value {
    let base = Path::new(root);
    for name in JSON_CANDIDATES {
        if let Ok(text) = std::fs::read_to_string(base.join(name)) {
            if let Some(v) = from_json_config(&text, rule) {
                return v;
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(base.join("package.json")) {
        if let Ok(pkg) = serde_json::from_str::<Value>(&text) {
            if let Some(setting) = pkg.get("eslintConfig").and_then(|c| c.get("rules")).and_then(|r| r.get(rule)) {
                return normalise(setting);
            }
        }
    }
    for name in JS_CANDIDATES {
        if let Ok(text) = std::fs::read_to_string(base.join(name)) {
            if let Some(v) = from_js_config(&text, rule) {
                return v;
            }
        }
    }
    Value::Null
}

/// The rules a constitution law can defer to, read once per judge run. Keyed by the ESLint rule
/// name a seed law names in `config_json.project_rule.eslint`.
#[allow(non_snake_case)]
pub fn projectRules(root: &str) -> Value {
    let mut out = serde_json::Map::new();
    for rule in ["eqeqeq"] {
        let v = readEslintRule(root, rule);
        if !v.is_null() {
            out.insert(rule.to_string(), v);
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-project-rules-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_reacts_own_config_shape_with_named_level_constants() {
        let d = dir("react");
        std::fs::write(d.join(".eslintrc.js"), "const ERROR = 2;\nmodule.exports = {\n  rules: {\n    'no-var': ERROR,\n    eqeqeq: [ERROR, 'allow-null'],\n  },\n};\n").unwrap();
        assert_eq!(readEslintRule(d.to_str().unwrap(), "eqeqeq"), json!({ "level": "error", "option": "allow-null" }));
        assert_eq!(readEslintRule(d.to_str().unwrap(), "no-var"), json!({ "level": "error", "option": null }));
        assert_eq!(readEslintRule(d.to_str().unwrap(), "curly"), Value::Null, "not mentioned → the project did not say");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reads_json_and_package_json_shapes() {
        let d = dir("json");
        std::fs::write(d.join(".eslintrc.json"), r#"{ "rules": { "eqeqeq": ["error", { "null": "ignore" }] } }"#).unwrap();
        assert_eq!(readEslintRule(d.to_str().unwrap(), "eqeqeq"), json!({ "level": "error", "option": "allow-null" }));
        let _ = std::fs::remove_dir_all(&d);

        let d = dir("pkg");
        std::fs::write(d.join("package.json"), r#"{ "name": "x", "eslintConfig": { "rules": { "eqeqeq": "off" } } }"#).unwrap();
        assert_eq!(readEslintRule(d.to_str().unwrap(), "eqeqeq"), json!({ "level": "off", "option": null }));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn flat_config_and_always_option() {
        let d = dir("flat");
        std::fs::write(d.join("eslint.config.mjs"), "export default [{ rules: { eqeqeq: ['error', 'always'] } }];\n").unwrap();
        assert_eq!(readEslintRule(d.to_str().unwrap(), "eqeqeq"), json!({ "level": "error", "option": "always" }));
        assert_eq!(projectRules(d.to_str().unwrap()), json!({ "eqeqeq": { "level": "error", "option": "always" } }));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_config_is_null_not_off() {
        let d = dir("none");
        assert_eq!(readEslintRule(d.to_str().unwrap(), "eqeqeq"), Value::Null);
        assert_eq!(projectRules(d.to_str().unwrap()), json!({}));
        let _ = std::fs::remove_dir_all(&d);
    }
}
