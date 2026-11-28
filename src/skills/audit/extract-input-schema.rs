//! extract-input-schema.rs — twin of skills/audit/extract-input-schema.mjs. NAMES ARE CONTRACT:
//! `extractInputSchema` (default). Parses a skill's source (by name via `findSkillPath` + a real
//! `fs::read_to_string`, or inline `source`) and extracts the shape of its `input` parameter: keys
//! destructured off `input` (with required/default info) merged with plain `input.foo` member
//! access (required:false, no default info — destructured wins on overlap).
//!
//! BESPOKE-NATIVE (real fs read of a skill file via the already-converted
//! `engine_load_skill::findSkillPath` twin), not conform-db: the two-lane JSON-fixture harness has
//! no way to stage "read this real skill file off disk". Proven below via `#[cfg(test)]` against
//! inline `source` (no fs) plus one real-file case resolved through `findSkillPath`.
//!
//! ## Architecture: native `oxc_parser`, not the `parse-js.rs` atlas twin
//! `parse-js.rs`'s `parseJs` is a bespoke code-ATLAS extractor (function/call/import summaries for
//! `atlas-ingest`) — it does not expose a generic ESTree tree, and this skill's walk (`input`
//! destructuring + arbitrary `input.foo` member access anywhere in the file) needs exactly that.
//! So this twin parses directly with `oxc_parser` (already a workspace dependency, see
//! `parse-js.rs`'s own module doc for the crate-choice rationale) and drives an `oxc_ast_visit`
//! `Visit` walk that inspects every `VariableDeclarator` and every member expression — the native
//! equivalent of the JS source's hand-rolled generic `Object.keys` `walkAst`. The JS source imports
//! plain `acorn` (no `acorn-jsx` plugin, unlike `parse-js.mjs`), so this twin parses as a plain ESM
//! module (`SourceType::mjs()`), NOT `SourceType::jsx()`.
//!
//! ## ESTree shape differences bridged here (see `parse-js.rs` for the fuller catalogue)
//! - Acorn's one `MemberExpression{computed}` is oxc's `StaticMemberExpression` (property is an
//!   `IdentifierName`) vs `ComputedMemberExpression` (property is an `Expression`) — both are
//!   walked here, matching `memberAccessField`'s computed/non-computed branches.
//! - Acorn's `ChainExpression` (optional-chain wrapper) carries no extra info this walk needs: oxc
//!   puts the `optional` flag directly on the member-expression node itself and the `Visit` walk
//!   descends into a `ChainExpression`'s inner element like any other node, so `input?.foo` is
//!   found by the same `visit_static_member_expression` override with no special-casing — the JS
//!   source's `obj.type === 'ChainExpression'` unwrap branch never actually fires for ESTree's
//!   single-outer-wrapper shape and has no counterpart needed here.
//! - Acorn's one `Literal{value}` (used by `summarizeDefault`) is split across oxc's per-kind
//!   literal expressions; only the kinds realistically found as a destructuring default (string,
//!   number, boolean, null, regex) are handled — same rationale as `parse-js.rs`'s `is_literal_like`.
//! - `PropertyKey` on an `ObjectPattern`'s `BindingProperty`: only `StaticIdentifier` (`{foo}`) and
//!   `StringLiteral` (`{'foo': x}`) are resolved to a name, matching the JS source's
//!   `prop.key.name ?? prop.key.value`; any other computed key returns `None` (skip), same as the
//!   JS source's `if (sourceKey == null) continue`.
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, ComputedMemberExpression, Expression, LogicalOperator, ObjectPattern, Program,
    StaticMemberExpression, UnaryOperator, VariableDeclarator,
};
use oxc_ast_visit::{walk, Visit};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn is_input_expression(expr: &Expression) -> bool {
    match expr {
        Expression::Identifier(id) => id.name == "input",
        Expression::LogicalExpression(le) => {
            matches!(le.operator, LogicalOperator::Coalesce | LogicalOperator::Or)
                && is_input_expression(&le.left)
        }
        _ => false,
    }
}

fn summarize_default(expr: &Expression) -> Value {
    match expr {
        Expression::StringLiteral(s) => json!(s.value.as_str()),
        Expression::NumericLiteral(n) => number_value(n.value),
        Expression::BooleanLiteral(b) => json!(b.value),
        Expression::NullLiteral(_) => Value::Null,
        Expression::RegExpLiteral(_) => json!({}), // JSON.stringify(/re/) === '{}'
        Expression::ArrayExpression(_) => json!([]),
        Expression::ObjectExpression(_) => json!({}),
        Expression::Identifier(id) => json!(format!("<{}>", id.name.as_str())),
        Expression::UnaryExpression(u) if u.operator == UnaryOperator::UnaryNegation => {
            match &u.argument {
                Expression::NumericLiteral(n) => number_value(-n.value),
                _ => json!("<expression>"),
            }
        }
        _ => json!("<expression>"),
    }
}

// JS Number values stringify without a trailing ".0" for whole numbers.
fn number_value(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

fn property_key_name(key: &oxc_ast::ast::PropertyKey) -> Option<String> {
    use oxc_ast::ast::PropertyKey;
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str().to_string()),
        PropertyKey::StringLiteral(s) => Some(s.value.as_str().to_string()),
        _ => None,
    }
}

fn fields_from_object_pattern(pattern: &ObjectPattern) -> Vec<(String, Value)> {
    let mut fields = Vec::new();
    for prop in &pattern.properties {
        let Some(source_key) = property_key_name(&prop.key) else { continue };
        let (required, default) = match &prop.value {
            BindingPattern::AssignmentPattern(ap) => (false, Some(summarize_default(&ap.right))),
            _ => (true, None),
        };
        let mut m = Map::new();
        m.insert("name".into(), json!(source_key.clone()));
        m.insert("required".into(), json!(required));
        if let Some(d) = default {
            m.insert("default".into(), d);
        }
        fields.push((source_key, Value::Object(m)));
    }
    fields
}

#[derive(Default)]
struct SchemaVisitor {
    destructured: Vec<(String, Value)>,
    member_access: Vec<String>,
    member_seen: HashSet<String>,
}

impl SchemaVisitor {
    fn record_member(&mut self, name: String) {
        if self.member_seen.insert(name.clone()) {
            self.member_access.push(name);
        }
    }
}

impl<'a> Visit<'a> for SchemaVisitor {
    fn visit_variable_declarator(&mut self, it: &VariableDeclarator<'a>) {
        if let BindingPattern::ObjectPattern(op) = &it.id {
            if let Some(init) = &it.init {
                if is_input_expression(init) {
                    self.destructured.extend(fields_from_object_pattern(op));
                }
            }
        }
        walk::walk_variable_declarator(self, it);
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        if is_input_expression(&it.object) {
            self.record_member(it.property.name.as_str().to_string());
        }
        walk::walk_static_member_expression(self, it);
    }

    fn visit_computed_member_expression(&mut self, it: &ComputedMemberExpression<'a>) {
        if is_input_expression(&it.object) {
            if let Expression::StringLiteral(s) = &it.expression {
                self.record_member(s.value.as_str().to_string());
            }
        }
        walk::walk_computed_member_expression(self, it);
    }
}

fn merge_fields(destructured: Vec<(String, Value)>, member_access: Vec<String>) -> Value {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (name, v) in destructured {
        seen.insert(name);
        out.push(v);
    }
    for name in member_access {
        if !seen.contains(&name) {
            seen.insert(name.clone());
            out.push(json!({ "name": name, "required": false }));
        }
    }
    Value::Array(out)
}

fn walk_source(source: &str) -> Value {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, SourceType::mjs())
        .with_options(ParseOptions { preserve_parens: false, ..ParseOptions::default() })
        .parse();
    let program: Program = ret.program;
    let mut v = SchemaVisitor::default();
    v.visit_program(&program);
    merge_fields(v.destructured, v.member_access)
}

/// extractInputSchema(input, context) — default export, standard lane; BESPOKE-NATIVE (real fs
/// read via the `findSkillPath` twin when `source` is not given inline).
#[allow(non_snake_case)]
pub fn extractInputSchema(input: &Value, _db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();

    let inline_source = obj.and_then(|o| o.get("source")).and_then(|v| v.as_str());

    let name: String = if let Some(s) = input.as_str() {
        s.to_string()
    } else if inline_source.is_some() {
        obj.and_then(|o| o.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "<inline>".to_string())
    } else if let Some(n) = obj.and_then(|o| o.get("name")).and_then(|v| v.as_str()) {
        n.to_string()
    } else {
        return Err("extract-input-schema: name or source is required".into());
    };

    let mut path: Option<String> = None;
    let source: String = if let Some(s) = inline_source {
        s.to_string()
    } else {
        let p = crate::engine_load_skill::findSkillPath(&json!(name))?;
        let p_str = p.as_str().unwrap_or_default().to_string();
        let src = std::fs::read_to_string(&p_str)
            .map_err(|e| format!("extract-input-schema: {e}"))?;
        path = Some(p_str);
        src
    };

    // RUST SOURCE, RUST EXTRACTION. `walk_source` parses JavaScript with oxc and reads the
    // fields off `input.foo` member expressions and JSDoc `@param` tags. A `.rs` file has neither
    // — and it does not degrade, it fails to parse and reports every skill as having no inputs.
    //
    // extract-rust-skill-meta derives the same fact from `input.get("field")` call sites, which is
    // strictly better than what it replaces: a JSDoc tag could drift from the code, a call site is
    // the code. The JS path is kept for an inline `source` that still looks like JavaScript, so a
    // caller passing JS text by hand gets the old behaviour.
    let looks_like_rust = source.contains("//!")
        || source.contains("pub fn ")
        || path.as_deref().is_some_and(|p| p.ends_with(".rs"));
    let fields = if looks_like_rust {
        crate::audit_extract_rust_skill_meta::extractRustInputSchema(&source)
    } else {
        walk_source(&source)
    };

    Ok(json!({
        "name": name,
        "path": path,
        "fields": fields,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn destructured_required_and_default_fields() {
        let source = "export default function foo(input, context) {\n  const { name, count = 3, label = 'x' } = input;\n  return { name, count, label };\n}\n";
        let r = extractInputSchema(&json!({ "source": source, "name": "foo" }), &db()).unwrap();
        assert_eq!(r["name"], json!("foo"));
        assert_eq!(r["path"], Value::Null);
        let fields = r["fields"].as_array().unwrap();
        assert_eq!(
            fields[0],
            json!({ "name": "name", "required": true })
        );
        assert_eq!(
            fields[1],
            json!({ "name": "count", "required": false, "default": 3 })
        );
        assert_eq!(
            fields[2],
            json!({ "name": "label", "required": false, "default": "x" })
        );
    }

    #[test]
    fn member_access_fields_and_optional_chain() {
        let source = "export default function foo(input) {\n  return input.a + input?.b + input.c;\n}\n";
        let r = extractInputSchema(&json!({ "source": source }), &db()).unwrap();
        let fields = r["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 3);
        for f in fields {
            assert_eq!(f["required"], json!(false));
            assert!(f.get("default").is_none());
        }
        let names: Vec<&str> = fields.iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn destructured_wins_over_member_access_on_overlap() {
        let source = "export default function foo(input) {\n  const { x } = input;\n  return input.x + input.y;\n}\n";
        let r = extractInputSchema(&json!({ "source": source }), &db()).unwrap();
        let fields = r["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0], json!({ "name": "x", "required": true }));
        assert_eq!(fields[1], json!({ "name": "y", "required": false }));
    }

    #[test]
    fn nullish_and_or_default_object_wrappers_still_resolve_member_access() {
        let source = "export default function foo(input) {\n  const a = (input ?? {}).a;\n  const b = (input || {}).b;\n  return a + b;\n}\n";
        let r = extractInputSchema(&json!({ "source": source }), &db()).unwrap();
        let fields = r["fields"].as_array().unwrap();
        let names: Vec<&str> = fields.iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn computed_member_with_string_literal_key_resolves() {
        let source = "export default function foo(input) {\n  return input['weird-key'];\n}\n";
        let r = extractInputSchema(&json!({ "source": source }), &db()).unwrap();
        assert_eq!(r["fields"][0]["name"], json!("weird-key"));
    }

    #[test]
    fn resolves_real_skill_by_name_via_find_skill_path() {
        // load-skill's CACHE is process-global and its own tests repoint it at a temp ANTILOKI_HOME.
        // This test needs the REAL skills root, so it takes the same lock and drops their cache.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        crate::engine_load_skill::clearCache();
        let r = extractInputSchema(&json!("card-add"), &db()).unwrap();
        assert_eq!(r["name"], json!("card-add"));
        assert!(r["path"].as_str().unwrap().ends_with("card-add.rs"));
        assert!(!r["fields"].as_array().unwrap().is_empty());
    }

    #[test]
    fn missing_name_and_source_errors() {
        assert!(extractInputSchema(&json!({}), &db()).is_err());
    }

    #[test]
    fn array_and_object_and_negative_number_defaults() {
        let source = "export default function foo(input) {\n  const { a = [], b = {}, c = -5 } = input;\n  return a + b + c;\n}\n";
        let r = extractInputSchema(&json!({ "source": source }), &db()).unwrap();
        let fields = r["fields"].as_array().unwrap();
        assert_eq!(fields[0]["default"], json!([]));
        assert_eq!(fields[1]["default"], json!({}));
        assert_eq!(fields[2]["default"], json!(-5));
    }
}
