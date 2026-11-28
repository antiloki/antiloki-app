//! lint-skill.rs — twin of skills/audit/lint-skill.mjs. NAMES ARE CONTRACT:
//! `lintSubskill` kept exactly as the JS source names its default export.
//!
//! ## Parser: native `oxc_parser`, parsed INLINE
//! The JS source parses with `acorn` (`ACORN_OPTS = { ecmaVersion: 'latest', sourceType: 'module' }`)
//! purely to find `ast.body`'s `ExportDefaultDeclaration`(s) and check whether the declaration is a
//! function with exactly 2 params. This twin follows the established `parse-js.rs`/
//! `extract-jsdoc-description.rs` architecture choice (`oxc_parser` 0.143, parsed+walked inline in
//! one function scope — the arena-allocated AST is lifetime-bound to its `Allocator`).
//! `SourceType::jsx()` (JS+Module+JSX, matching this crate's convention for plain `.mjs` skill
//! source) is used rather than a bare `SourceType::mjs()` — skill source here is always plain JS,
//! so this is a difference without effect, but stays consistent with every other oxc-backed twin
//! in this crate.
//!
//! ## acorn's `isFunctionNode` (3 flat type strings) vs oxc's split `ExportDefaultDeclarationKind`
//! The JS source's `isFunctionNode` checks `node.type` against `'FunctionDeclaration'` /
//! `'FunctionExpression'` / `'ArrowFunctionExpression'`. oxc's `ExportDefaultDeclarationKind` only
//! has THREE named variants of its own — `FunctionDeclaration`, `ClassDeclaration`,
//! `TSInterfaceDeclaration` — plus a generated `INHERIT(Expression)` catch-all covering every other
//! expression shape (object/array literal, arrow function, plain function EXPRESSION, identifier,
//! call, …), reached via `.to_expression()` (verified against the oxc_ast 0.143.0 source: it
//! panics if called on one of the three named variants, so `classify_default_decl` below only
//! calls it in the fallback arm — same guard order `file-internal-diagram.rs`'s own
//! `edd.declaration` match uses for the identical enum). `ArrowFunctionExpression` and
//! `FunctionExpression` are therefore matched as `Expression` variants inside that fallback, not as
//! direct `ExportDefaultDeclarationKind` variants — the reachable-syntax set is identical to the JS
//! source's three-string check either way (a `class`/literal/object default export correctly falls
//! through to `DEFAULT_NOT_FUNCTION`, same as the JS source).
//!
//! ## Params length across the function-node kinds
//! `FunctionDeclaration`/`FunctionExpression`/`ArrowFunctionExpression` all carry the same
//! `Box<FormalParameters>` shape (`.items.len()`, ignoring `.rest` — the JS source's own
//! `defaultDecl.params.length` counts a rest param as ONE param too, via acorn's inline
//! `RestElement` in the params array, so this twin adds 1 when `.rest.is_some()` to match).
//!
//! ## `findSkillPath` + `readFileSync` — reused via the real `load-skill.rs` twin
//! `crate::engine_load_skill::findSkillPath` is a REAL (non-deferred) ported fs-walk (see that
//! module's own doc) — called here exactly like `extract-jsdoc-description.rs` does, then the
//! resolved path is read with `std::fs::read_to_string` (mirrors `readFileSync(path, 'utf8')`).
//!
//! ## Error shape
//! The JS source's only thrown error (missing `name`/`source`) is an `AntilokiError` with
//! `code: VALIDATION_REQUIRED, skill: 'lint-skill', cause: 'name or source is required'` — per
//! `engine/error.mjs`'s message-building rule (`skill` present + `cause` present →
//! `"{skill}: {cause}"`), this becomes the Rust `Err("lint-skill: name or source is required")`.
//! A parse failure is NOT a thrown error in the JS source — it's caught and turned into an
//! `issues: [{code: 'PARSE_ERROR', ...}]`, `valid: false` return value; ported the same way here.
// QUALITY: genuine full port — every lint check + input-shape branch + error path reproduced from
// the JS source line-by-line; oxc parse errors and lint issues both surface as Ok(json) returns,
// matching the JS source's own try/catch-and-return-issues structure exactly.
use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, ExportDefaultDeclarationKind, FormalParameters, Statement};
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde_json::{json, Value};

fn truthy_str(v: Option<&Value>) -> Option<&str> {
    match v {
        Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
        _ => None,
    }
}

fn params_len(params: &FormalParameters) -> usize {
    params.items.len() + if params.rest.is_some() { 1 } else { 0 }
}

// isFunctionNode(node): matches FunctionDeclaration / FunctionExpression / ArrowFunctionExpression.
// Returns None for anything else (class, literal, identifier, …), matching the JS source's
// `node?.type || 'unknown'` fallback used in the DEFAULT_NOT_FUNCTION message.
//
// oxc's `ExportDefaultDeclarationKind` only has THREE named variants of its own —
// `FunctionDeclaration`, `ClassDeclaration`, `TSInterfaceDeclaration` — plus a catch-all `INHERIT`
// wildcard covering every other `Expression` shape (object/array literal, arrow function, plain
// function EXPRESSION, identifier, call, …), reached via `.to_expression()` (panics if called on
// one of the three named variants, so only used in the fallback arm below — same guard order
// `file-internal-diagram.rs`'s own `edd.declaration` match uses). A plain
// `export default function(input, context) {}` (anonymous, no name) still parses to the SAME
// `FunctionDeclaration` variant as a named one (ESTree gives it `id: null`), so no separate case is
// needed for that; `export default (input, context) => …` and `export default function
// bar(input,context){}` used as an EXPRESSION (not a declaration — not reachable in real syntax,
// but oxc's Expression enum still has the variant) both live under the `INHERIT`/`to_expression()`
// fallback.
enum DeclKind<'a> {
    Function(&'a FormalParameters<'a>),
    NotFunction(&'static str),
}

fn classify_default_decl<'a>(kind: &'a ExportDefaultDeclarationKind<'a>) -> DeclKind<'a> {
    match kind {
        ExportDefaultDeclarationKind::FunctionDeclaration(f) => DeclKind::Function(&f.params),
        ExportDefaultDeclarationKind::ClassDeclaration(_) => DeclKind::NotFunction("ClassDeclaration"),
        ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => {
            DeclKind::NotFunction("TSInterfaceDeclaration")
        }
        _ => match kind.to_expression() {
            Expression::ArrowFunctionExpression(af) => DeclKind::Function(&af.params),
            Expression::FunctionExpression(fe) => DeclKind::Function(&fe.params),
            other => DeclKind::NotFunction(expr_kind_name(other)),
        },
    }
}

// Mirrors acorn's ESTree type-name strings for the JS source's `defaultDecl?.type` interpolation,
// for the Expression shapes reachable via `ExportDefaultDeclarationKind`'s `INHERIT` fallback
// (object/array literal, identifier, call, …) — unknown falls back to "unknown", matching the JS
// source's own `|| 'unknown'`.
fn expr_kind_name(expr: &Expression) -> &'static str {
    match expr {
        Expression::StringLiteral(_)
        | Expression::NumericLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::RegExpLiteral(_) => "Literal",
        Expression::Identifier(_) => "Identifier",
        Expression::ObjectExpression(_) => "ObjectExpression",
        Expression::ArrayExpression(_) => "ArrayExpression",
        Expression::CallExpression(_) => "CallExpression",
        _ => "unknown",
    }
}


/// `card-add` → `cardAdd`. The file-name-to-function convention every skill in this crate follows.
fn camel_case_of(name: &str) -> String {
    let mut out = String::new();
    let mut upper_next = false;
    for ch in name.chars() {
        if ch == '-' || ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Does the source declare `pub fn <want>`? Tolerates the `#[allow(non_snake_case)]` attribute and
/// any whitespace, and requires a non-identifier character after the name so `cardAdd` does not
/// match `cardAddChild`.
fn declares_fn(body: &str, want: &str) -> bool {
    let needle = format!("pub fn {want}");
    let mut from = 0usize;
    while let Some(i) = body[from..].find(&needle) {
        let at = from + i;
        let after = body[at + needle.len()..].chars().next();
        if !matches!(after, Some(c) if c.is_alphanumeric() || c == '_') {
            return true;
        }
        from = at + needle.len();
    }
    false
}

/// The skill-contract lint. Named for the JS default export it began as.
#[allow(non_snake_case)]
pub fn lintSubskill(input: &Value, _context: &Value) -> Result<Value, String> {
    let (name, mut source): (String, Option<String>) = if let Value::String(s) = input {
        (s.clone(), None)
    } else if let Some(src) = truthy_str(input.get("source")) {
        let name = truthy_str(input.get("name")).unwrap_or("<inline>").to_string();
        (name, Some(src.to_string()))
    } else if let Some(nm) = truthy_str(input.get("name")) {
        (nm.to_string(), None)
    } else {
        return Err("lint-skill: name or source is required".to_string());
    };

    let mut path: Value = Value::Null;
    if source.is_none() {
        let resolved = crate::engine_load_skill::findSkillPath(&json!(name))?;
        let path_str = resolved.as_str().unwrap_or_default().to_string();
        let contents =
            std::fs::read_to_string(&path_str).map_err(|e| format!("lint-skill: {e}"))?;
        path = json!(path_str);
        source = Some(contents);
    }
    let source = source.unwrap();

    let mut issues: Vec<Value> = Vec::new();

    // THE CONTRACT IS RUST'S NOW. This used to parse JavaScript with oxc and assert exactly one
    // `export default` of a 2-param function — the shape SKILL_EXPORTS_DEFAULT_FUNCTION describes.
    // A Rust skill has no `export default`, so the law's INTENT is re-expressed in the three facts
    // that actually make a Rust skill dispatchable:
    //
    //   1. it declares its contract function — `pub fn <camelCaseOfFileName>`;
    //   2. that function is REGISTERED in skill_registry, or nothing can dispatch it;
    //   3. it carries a `//!` module doc, which is what the catalog shows the operator.
    //
    // (2) is the one the JS lint could never check, because `import()` resolved by filename: a JS
    // skill was dispatchable by existing. In Rust a skill can be perfect, compiled, and still
    // unreachable — exactly what happened to git-pending-list, which sat in the tree with a
    // bespoke signature and no registry arm until a fresh-workspace 404 surfaced it.
    // Tests live INSIDE the file in Rust, so the contract fn must be found before them: a
    // `pub fn cardAdd` declared only in `mod tests` does not make the skill dispatchable.
    let test_marker = "#[cfg(test)";
    let body = match source.find(test_marker) {
        Some(i) => &source[..i],
        None => source.as_str(),
    };

    let expected_fn = camel_case_of(&name);
    if !declares_fn(body, &expected_fn) {
        issues.push(json!({
            "code": "NO_CONTRACT_FN",
            "message": format!("skill must declare `pub fn {expected_fn}` (NAMES ARE CONTRACT)"),
        }));
    }

    if !crate::skill_registry::is_registered(&name) {
        issues.push(json!({
            "code": "NOT_REGISTERED",
            "message": format!(
                "'{name}' has no arm in skill_registry — it compiles but nothing can dispatch it"
            ),
        }));
    }

    if crate::audit_extract_rust_skill_meta::extractRustDescription(&source).is_none() {
        issues.push(json!({
            "code": "NO_MODULE_DOC",
            "message": "skill must open with a `//!` module doc — the catalog shows it to the operator",
        }));
    }

    Ok(json!({
        "name": name,
        "path": path,
        "valid": issues.is_empty(),
        "issues": issues,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A skill that satisfies all three facts. `card-add` is a REAL registered name, so the
    /// registry check runs against the real table rather than a stub.
    const GOOD: &str = "//! card-add.rs — add a card.\npub fn cardAdd(input: &Value) {}\n";

    fn codes(out: &Value) -> Vec<String> {
        out["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["code"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn missing_name_and_source_errors() {
        let e = lintSubskill(&json!({}), &Value::Null).unwrap_err();
        assert!(e.contains("name or source is required"), "{e}");
    }

    #[test]
    fn a_conforming_skill_reports_no_issues() {
        let out = lintSubskill(&json!({ "name": "card-add", "source": GOOD }), &Value::Null).unwrap();
        assert_eq!(out["valid"], json!(true), "{}", out["issues"]);
        assert_eq!(out["issues"], json!([]));
    }

    #[test]
    fn a_missing_contract_fn_is_flagged_with_the_name_it_expected() {
        // NAMES ARE CONTRACT: card-add.rs must declare `pub fn cardAdd`.
        let src = "//! card-add.rs — add a card.\npub fn somethingElse(input: &Value) {}\n";
        let out = lintSubskill(&json!({ "name": "card-add", "source": src }), &Value::Null).unwrap();
        assert_eq!(out["valid"], json!(false));
        assert!(codes(&out).contains(&"NO_CONTRACT_FN".to_string()), "{:?}", codes(&out));
        assert!(out["issues"][0]["message"].as_str().unwrap().contains("cardAdd"));
    }

    #[test]
    fn an_UNREGISTERED_skill_is_flagged_even_when_otherwise_perfect() {
        // The check the JS lint could never make: in JavaScript a skill was dispatchable by
        // existing on disk. In Rust it can compile, be well-formed, and still be unreachable —
        // which is exactly what happened to git-pending-list.
        let src = "//! not-a-real-skill.rs — fine in every other way.\npub fn notARealSkill(input: &Value) {}\n";
        let out = lintSubskill(&json!({ "name": "not-a-real-skill", "source": src }), &Value::Null).unwrap();
        assert!(codes(&out).contains(&"NOT_REGISTERED".to_string()), "{:?}", codes(&out));
    }

    #[test]
    fn a_skill_with_no_module_doc_is_flagged() {
        let src = "pub fn cardAdd(input: &Value) {}\n";
        let out = lintSubskill(&json!({ "name": "card-add", "source": src }), &Value::Null).unwrap();
        assert!(codes(&out).contains(&"NO_MODULE_DOC".to_string()), "{:?}", codes(&out));
    }

    #[test]
    fn a_TEST_ONLY_declaration_does_not_satisfy_the_contract() {
        // The contract fn has to be in the skill, not in its test module.
        let src = "//! card-add.rs — add a card.\n#[cfg(test)]\nmod tests { pub fn cardAdd() {} }\n";
        let out = lintSubskill(&json!({ "name": "card-add", "source": src }), &Value::Null).unwrap();
        assert!(codes(&out).contains(&"NO_CONTRACT_FN".to_string()), "{:?}", codes(&out));
    }

    #[test]
    fn a_PREFIX_match_does_not_count_as_the_contract_fn() {
        // `cardAddChild` must not satisfy `card-add`.
        let src = "//! card-add.rs — add a card.\npub fn cardAddChild(input: &Value) {}\n";
        let out = lintSubskill(&json!({ "name": "card-add", "source": src }), &Value::Null).unwrap();
        assert!(codes(&out).contains(&"NO_CONTRACT_FN".to_string()), "{:?}", codes(&out));
    }

    #[test]
    fn camel_case_conversion_matches_the_naming_convention() {
        assert_eq!(camel_case_of("card-add"), "cardAdd");
        assert_eq!(camel_case_of("with-audit"), "withAudit");
        assert_eq!(camel_case_of("law-hook-no-lone-skill-dispatch"), "lawHookNoLoneSkillDispatch");
        assert_eq!(camel_case_of("embed"), "embed");
    }

    #[test]
    fn a_string_input_resolves_through_find_skill_path() {
        // Against the REAL tree: card-add.rs exists, declares cardAdd, is registered, is documented.
        //
        // ENV_LOCK because this READS the ambient ANTILOKI_SKILLS_ROOT while other tests SET it.
        // A reader that skips the lock is the exact flake this crate has paid for repeatedly: the
        // writers serialize with each other and then break whoever is reading unguarded.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        crate::engine_load_skill::clearCache();
        let out = lintSubskill(&json!("card-add"), &Value::Null).unwrap();
        assert!(out["path"].as_str().unwrap().ends_with("card-add.rs"), "{}", out["path"]);
        assert_eq!(out["valid"], json!(true), "{}", out["issues"]);
    }

    #[test]
    fn an_unresolvable_name_errors() {
        assert!(lintSubskill(&json!("no-such-skill-anywhere"), &Value::Null).is_err());
    }
}
