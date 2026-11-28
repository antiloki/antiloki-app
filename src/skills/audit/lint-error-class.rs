//! lint-error-class.rs — twin of skills/audit/lint-error-class.mjs, the AST analyzer that finds
//! plain `throw new Error(...)` throws (the codebase migrated to AntilokiError / the `err.*`
//! helpers). NAMES ARE CONTRACT: `lintErrorClass` (default) · `ALLOWED` (named).
//!
//! ## acorn → oxc
//! The JS parses each skill with `acorn.parse(source, {ecmaVersion:'latest', sourceType:'module',
//! locations:true})` and hand-rolls a generic `Object.keys` walk. This twin uses `oxc_parser` +
//! `oxc_ast_visit::Visit` — the same swap `parse-js.rs`/`extract-anatomy.rs` already made, and for
//! the same reason: the generated `Visit` trait IS the recursive descent, so there is no
//! hand-written walker to drift from the real grammar. `SourceType::jsx()` matches the crate
//! convention for `.mjs` skill source (see lint-skill.rs's module doc).
//!
//! ## Positions
//! acorn's `loc.start` is a **1-based line** and a **0-based column counted in UTF-16 code units**.
//! oxc reports byte offsets, so `line_col_utf16` converts. The UTF-16 detail is not pedantry here:
//! this repo's sources are full of em dashes and arrows in comments and strings, and a byte-count
//! column would drift on every line after one.
//!
//! ## Parse errors
//! JS catches acorn's throw and records `{code:'PARSE_ERROR', message: err.message}`. oxc does not
//! throw — it returns diagnostics — so a non-empty `ret.errors` maps to the same issue shape, using
//! the FIRST diagnostic's message. The message text itself is parser-specific and therefore not
//! byte-comparable with acorn's; the issue's `code`/`skill`/`path` are.
use oxc_allocator::Allocator;
use rusqlite::Connection;
use oxc_ast::ast::{Expression, ThrowStatement};
use oxc_ast_visit::{walk, Visit};
use oxc_parser::Parser;
use oxc_span::SourceType;
use serde_json::{json, Value};

/// JS: `const ALLOWED = new Set(['error'])` — skills that legitimately throw a plain Error while
/// bootstrapping the error system itself (you cannot use AntilokiError to validate AntilokiError).
pub const ALLOWED: [&str; 1] = ["error"];

/// 1-based line + 0-based UTF-16 column for a byte offset, matching acorn's `loc.start`.
fn line_col_utf16(source: &str, offset: usize) -> (i64, i64) {
    let mut line = 1i64;
    let mut col = 0i64;
    for (idx, ch) in source.char_indices() {
        if idx >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16() as i64;
        }
    }
    (line, col)
}

#[derive(Default)]
struct PlainErrorThrows {
    offsets: Vec<usize>,
}

impl<'a> Visit<'a> for PlainErrorThrows {
    fn visit_throw_statement(&mut self, it: &ThrowStatement<'a>) {
        // JS: n.argument?.type === 'NewExpression' && callee.type === 'Identifier' && name === 'Error'
        if let Expression::NewExpression(new_expr) = &it.argument {
            if let Expression::Identifier(id) = &new_expr.callee {
                if id.name == "Error" {
                    self.offsets.push(it.span.start as usize);
                }
            }
        }
        walk::walk_throw_statement(self, it);
    }
}

fn find_plain_error_throws(source: &str) -> Result<Vec<(i64, i64)>, String> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, SourceType::jsx()).parse();
    if ret.diagnostics.has_errors() {
        return Err(ret
            .diagnostics
            .errors()
            .next()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "parse error".to_string()));
    }
    let mut v = PlainErrorThrows::default();
    v.visit_program(&ret.program);
    Ok(v.offsets.into_iter().map(|o| line_col_utf16(source, o)).collect())
}

/// `export default function lintErrorClass(input, _context)`.
pub fn lintErrorClass(input: &Value, _db: &Connection) -> Result<Value, String> {
    // JS: `input?.only ?? null`, then `all.filter((s) => onlyNames.includes(s.name))`.
    let only: Option<Vec<String>> = input
        .get("only")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());

    let all = crate::engine_load_skill::listSkills();
    let targets: Vec<Value> = all
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| match &only {
            None => true,
            Some(names) => s
                .get("name")
                .and_then(Value::as_str)
                .map(|n| names.iter().any(|x| x == n))
                .unwrap_or(false),
        })
        .collect();

    let mut issues: Vec<Value> = Vec::new();
    let mut checked: Vec<Value> = Vec::new();

    for t in &targets {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let path = t.get("path").and_then(Value::as_str).unwrap_or("").to_string();

        if ALLOWED.contains(&name.as_str()) {
            checked.push(json!({ "name": name, "plainErrorThrows": 0, "exempted": true }));
            continue;
        }

        // JS readFileSync throws on an unreadable path and does NOT catch it — the catch only wraps
        // the parse. A read failure there propagates; here it surfaces as the same kind of Err.
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("lint-error-class: cannot read '{path}': {e}"))?;

        match find_plain_error_throws(&source) {
            Err(msg) => {
                issues.push(json!({ "skill": name, "path": path, "code": "PARSE_ERROR", "message": msg }));
                continue;
            }
            Ok(throws) => {
                checked.push(json!({ "name": name, "plainErrorThrows": throws.len() }));
                for (line, column) in throws {
                    issues.push(json!({
                        "skill": name,
                        "path": path,
                        "code": "PLAIN_ERROR_THROW",
                        "message": format!("{name}: 'throw new Error(...)' at line {line} — use AntilokiError or err.* helpers"),
                        "line": line,
                        "column": column,
                    }));
                }
            }
        }
    }

    Ok(json!({
        "checked": checked.len(),
        "valid": issues.is_empty(),
        "violations": issues.len(),
        "issues": issues,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_top_level_plain_error_throw_with_acorn_positions() {
        let src = "function f() {\n  throw new Error('boom');\n}\n";
        let hits = find_plain_error_throws(src).unwrap();
        // line is 1-based, column 0-based — `throw` starts at col 2 on line 2
        assert_eq!(hits, vec![(2, 2)]);
    }

    #[test]
    fn finds_throws_nested_in_blocks_closures_and_catch() {
        let src = "const g = () => {\n  try {\n    if (x) { throw new Error('a'); }\n  } catch {\n    throw new Error('b');\n  }\n};\n";
        let hits = find_plain_error_throws(src).unwrap();
        assert_eq!(hits.len(), 2, "the generated Visit must recurse into arrow/try/if bodies");
        assert_eq!(hits[0].0, 3);
        assert_eq!(hits[1].0, 5);
    }

    #[test]
    fn ignores_antiloki_error_and_err_helper_throws() {
        let src = "throw new AntilokiError({ code: 'X' });\nthrow err.required('s', 'f');\nthrow new TypeError('t');\n";
        assert_eq!(find_plain_error_throws(src).unwrap().len(), 0);
    }

    #[test]
    fn ignores_a_non_throw_new_error_expression() {
        // `new Error(...)` that is merely constructed (not thrown) is not a violation
        let src = "const e = new Error('not thrown');\nreject(new Error('nor this'));\n";
        assert_eq!(find_plain_error_throws(src).unwrap().len(), 0);
    }

    #[test]
    fn column_counts_utf16_units_so_an_em_dash_does_not_drift_it() {
        // one em dash (3 UTF-8 bytes, 1 UTF-16 unit) before the throw on the same line
        let src = "const s = '—'; throw new Error('x');\n";
        let hits = find_plain_error_throws(src).unwrap();
        let byte_col = src.find("throw").unwrap() as i64; // 17 — the em dash is 3 UTF-8 bytes
        assert_eq!(hits[0].0, 1);
        assert_eq!(hits[0].1, 15, "UTF-16 column, as acorn reports it");
        assert_eq!(byte_col, 17);
        assert_ne!(hits[0].1, byte_col, "a byte column would drift by 2 here — the bug this guards");
    }

    #[test]
    fn syntax_error_is_reported_not_thrown() {
        let e = find_plain_error_throws("function ( {").unwrap_err();
        assert!(!e.is_empty());
    }

    #[test]
    fn jsx_source_parses_rather_than_failing() {
        let src = "const C = () => <div onClick={() => { throw new Error('jsx'); }} />;\n";
        assert_eq!(find_plain_error_throws(src).unwrap().len(), 1);
    }

    #[test]
    fn allowed_exempts_the_error_bootstrap_skill() {
        assert!(ALLOWED.contains(&"error"));
        assert!(!ALLOWED.contains(&"card-add"));
    }

    #[test]
    fn only_filter_narrows_the_target_set_and_shape_is_stable() {
        // `only` naming nothing real yields an empty, valid report — exercises the filter + shape
        // without depending on the real skills tree.
        let c = Connection::open_in_memory().unwrap();
        let r = lintErrorClass(&json!({ "only": ["__no_such_skill__"] }), &c).unwrap();
        assert_eq!(r["checked"], json!(0));
        assert_eq!(r["violations"], json!(0));
        assert_eq!(r["valid"], json!(true));
        assert_eq!(r["issues"], json!([]));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["checked", "valid", "violations", "issues"]);
    }
}
