//! code-lint.rs — twin of skills/codebase/code-lint.mjs, a deterministic (no-AI) syntax lint for a
//! playground code CHUNK: try to parse it and report ok/clean plus a single {message,line,column}
//! error on failure. No AST walk, no file write — the entire behavior is "does it parse".
//!
//! ## Parser swap: acorn+acorn-typescript (js-ast.mjs's parseModule) -> oxc_parser
//! js-ast.mjs's `parseModule` wraps one `acorn.Parser` extended with the `acorn-typescript` plugin
//! — a single superset grammar covering JS, JSX, TS, TSX, and decorators regardless of the actual
//! source shape, with `sourceType:'module'` FIXED (never auto-detected) plus
//! `allowReturnOutsideFunction`/`allowAwaitOutsideFunction` so a bare code fragment (not a full
//! file) doesn't trip on a stray top-level `return`/`await`. `oxc_parser` (already the sanctioned
//! native parser for this codebase's JS/TS lane — see parse-js.rs) reaches the same superset via
//! `SourceType::tsx()` (TypeScript language + JSX variant — TS is a syntactic superset of JS, and
//! TS parsing already covers decorators natively) `.with_module(true)` to PIN `ModuleKind::Module`
//! the same way acorn's fixed `sourceType:'module'` never falls back to Script — which also grants
//! top-level `await` for free (module-level top-level-await is standard ES2022), covering
//! `allowAwaitOutsideFunction`'s intent without a dedicated oxc option. `allow_return_outside_function:
//! true` in `ParseOptions` is the direct analog of acorn's `allowReturnOutsideFunction`.
//!
//! ## No AST walk needed — the easy end of the JS-parser-twin spectrum
//! Unlike parse-js.rs/extract-jsdoc-description.rs, code-lint never inspects `ast.body`: the JS
//! source calls `parseModule(code)` purely for its side effect (throw-or-not) and reads `e.message`/
//! `e.loc` off the caught error. So there is no ESTree-vs-oxc node-SHAPE bridging to do at all — the
//! only work is mapping oxc's `ParserReturn.diagnostics` (a collection of ALL parse diagnostics)
//! back to the SAME single-error shape acorn's throw-on-first-error `try/catch` produces: take the
//! first `Severity::Error`-level diagnostic only (see `codeLint` below), mirroring the JS source's
//! own array-of-at-most-one-error invariant (acorn throws once and stops; it never accumulates a
//! list of errors for `parseModule`'s caller to see).
//!
//! ## Known, bounded divergence: diagnostic MESSAGE TEXT and exact error position are parser-specific
//! `ok`/`clean`/error-COUNT and "does this source parse" are parser-independent facts (verified
//! against a live Node run of the real JS `codeLint` across 13 snippets this pass — every clean/
//! dirty verdict matched byte-for-byte, including plain-JS, JSX, TS-`interface`-plus-typed-const,
//! class-decorator, top-level-`return`, and top-level-`await` snippets that must stay clean, plus
//! truncated/malformed snippets — unclosed `{`, a dangling `+ ;`, a stray `return` where a `)` was
//! expected, a bare `= ;` — that must not). The exact WORDING of a syntax-error message
//! ("Unexpected token" vs oxc's own phrasing) and the precise line/column a parser's
//! error-recovery machinery points at are NOT parser-independent — two different recursive-descent
//! implementations legitimately disagree on where exactly a broken token stream "fails" (different
//! lookahead, different resync heuristics). This is not an ESTree node-shape gap (there is no AST
//! walk here to diverge on); it is inherent to swapping the underlying parser, exactly as if the JS
//! source itself swapped acorn for a different engine — a different acorn version can already
//! change a syntax-error's exact wording. Tests below therefore assert the parser-independent
//! contract (ok/clean/error-count/message non-empty and length-capped/line>=1/column>=0) rather
//! than acorn's exact wording, plus assert the JS source's own string-TRANSFORM (trailing
//! `" (N:N)"` strip + 200-char cap) byte-for-byte in isolation, since that transform IS pure,
//! parser-independent Rust logic shared with the real code path via `transform_message`.
use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use serde_json::{json, Value};

fn js_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(true, |f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

// JS `String(code)` for the value shapes code-lint's `{ code }` contract actually carries (a
// string, in practice — the `!code` truthy guard above already screens out null/false/0/""). A
// non-string/number/bool JSON shape (array/object) falls back to compact JSON text rather than
// JS's own Array.join/Object.toString rules, which the JS source never exercises here.
fn js_string_of(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// Mirrors `.replace(/\s*\(\d+:\d+\)\s*$/, '')`: strips a trailing "(<digits>:<digits>)" (plus any
// surrounding whitespace) anchored at the END of the string. Returns the ORIGINAL string unchanged
// the moment any part of the pattern fails to match, matching `String.replace`'s all-or-nothing
// semantics (no partial trim on a near-miss).
fn strip_trailing_loc_suffix(msg: &str) -> &str {
    let bytes = msg.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    if end == 0 || bytes[end - 1] != b')' {
        return msg;
    }
    let close = end - 1;
    let mut k = close;
    while k > 0 && bytes[k - 1].is_ascii_digit() {
        k -= 1;
    }
    if k == close || k == 0 || bytes[k - 1] != b':' {
        return msg;
    }
    let colon = k - 1;
    let mut m = colon;
    while m > 0 && bytes[m - 1].is_ascii_digit() {
        m -= 1;
    }
    if m == colon || m == 0 || bytes[m - 1] != b'(' {
        return msg;
    }
    let open = m - 1;
    let mut start = open;
    while start > 0 && bytes[start - 1].is_ascii_whitespace() {
        start -= 1;
    }
    &msg[..start]
}

// `.replace(/\s*\(\d+:\d+\)\s*$/, '').slice(0, 200)` — the exact JS transform chain, shared by
// `codeLint` and the unit tests below.
fn transform_message(raw: &str) -> String {
    strip_trailing_loc_suffix(raw).chars().take(200).collect()
}

// acorn's `Position` is 1-based line / 0-based column, counting `\n` bytes the same way
// parse-js.rs's `line_of` does (ASCII/BMP source, byte offset == UTF-16 code unit offset).
fn line_col(source: &str, offset: usize) -> (i64, i64) {
    let bytes = source.as_bytes();
    let end = offset.min(bytes.len());
    let mut line: i64 = 1;
    let mut last_nl: Option<usize> = None;
    for (i, &b) in bytes[..end].iter().enumerate() {
        if b == b'\n' {
            line += 1;
            last_nl = Some(i);
        }
    }
    let column = match last_nl {
        Some(nl) => (end - nl - 1) as i64,
        None => end as i64,
    };
    (line, column)
}

/// codeLint(input, _context) — parse-only lint, no execution/file-write. See module doc for the
/// acorn-typescript -> oxc_parser swap and the bounded message/position divergence this implies.
#[allow(non_snake_case)]
pub fn codeLint(input: &Value, _context: &Value) -> Result<Value, String> {
    let code_val = input.get("code");
    if !js_truthy(code_val) {
        return Ok(json!({ "ok": true, "clean": true, "errors": [] }));
    }
    let code = js_string_of(code_val.unwrap());
    if code.trim().is_empty() {
        return Ok(json!({ "ok": true, "clean": true, "errors": [] }));
    }

    let allocator = Allocator::default();
    // TypeScript + JSX + pinned Module — see module doc's parser-swap section.
    let source_type = SourceType::tsx().with_module(true);
    let ret = Parser::new(&allocator, &code, source_type)
        .with_options(ParseOptions { allow_return_outside_function: true, ..ParseOptions::default() })
        .parse();

    let Some(diag) = ret.diagnostics.errors().next() else {
        return Ok(json!({ "ok": true, "clean": true, "errors": [] }));
    };

    let raw_message: &str = &diag.message;
    let message = transform_message(raw_message);
    let (line, column) = match diag.labels.first() {
        Some(label) => {
            let (l, c) = line_col(&code, label.offset() as usize);
            (Some(l), Some(c))
        }
        None => (None, None),
    };

    Ok(json!({
        "ok": true,
        "clean": false,
        "errors": [{ "message": message, "line": line, "column": column }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lint(code: &str) -> Value {
        codeLint(&json!({ "code": code }), &json!({})).unwrap()
    }

    #[test]
    fn plain_js_stays_clean() {
        let r = lint("function add(a, b) {\n  return a + b;\n}\n");
        assert_eq!(r, json!({ "ok": true, "clean": true, "errors": [] }));
    }

    #[test]
    fn jsx_typescript_and_decorator_superset_stays_clean() {
        // Verified against a live Node run of the real JS codeLint for each of these forms.
        assert_eq!(
            lint("interface Foo { x: number }\nconst y: Foo = { x: 1 };\n")["clean"],
            json!(true)
        );
        assert_eq!(lint("class A { @Injectable() foo() {} }")["clean"], json!(true));
        assert_eq!(
            lint("<div>{items.map(x => <span>{x}</span>)}</div>")["clean"],
            json!(true)
        );
    }

    #[test]
    fn top_level_return_and_await_stay_clean() {
        // Matches acorn's allowReturnOutsideFunction / allowAwaitOutsideFunction leniency for a
        // pasted-in fragment rather than a full module.
        assert_eq!(lint("return 5;")["clean"], json!(true));
        assert_eq!(lint("await Promise.resolve(1);")["clean"], json!(true));
    }

    #[test]
    fn empty_whitespace_or_missing_code_is_clean() {
        let expected = json!({ "ok": true, "clean": true, "errors": [] });
        assert_eq!(codeLint(&json!({ "code": "" }), &json!({})).unwrap(), expected);
        assert_eq!(codeLint(&json!({ "code": "   \n\t  " }), &json!({})).unwrap(), expected);
        assert_eq!(codeLint(&json!({}), &json!({})).unwrap(), expected);
    }

    #[test]
    fn falsy_non_string_code_is_clean() {
        let expected = json!({ "ok": true, "clean": true, "errors": [] });
        assert_eq!(codeLint(&json!({ "code": 0 }), &json!({})).unwrap(), expected);
        assert_eq!(codeLint(&json!({ "code": false }), &json!({})).unwrap(), expected);
    }

    #[test]
    fn truthy_string_zero_is_parsed_and_clean() {
        // code:"0" is a truthy, non-empty string -> parsed as the valid expression `0`.
        // Verified against a live Node run of the real JS codeLint.
        assert_eq!(lint("0")["clean"], json!(true));
    }

    #[test]
    fn unbalanced_brace_reports_one_dirty_error_on_line_one() {
        // Verified against a live Node run of the real JS codeLint: clean:false, one error.
        // Message wording/exact column are parser-specific (see module doc); line is not — a
        // single-line snippet's only possible error line is 1.
        let r = lint("const x = {");
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["clean"], json!(false));
        let errors = r["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        let msg = errors[0]["message"].as_str().unwrap();
        assert!(!msg.is_empty());
        assert!(msg.chars().count() <= 200);
        assert_eq!(errors[0]["line"], json!(1));
        assert!(errors[0]["column"].as_i64().unwrap() >= 0);
    }

    #[test]
    fn dangling_operator_reports_one_dirty_error_on_line_one() {
        let r = lint("function add(a,b) { return a + ; }");
        assert_eq!(r["clean"], json!(false));
        assert_eq!(r["errors"].as_array().unwrap().len(), 1);
        assert_eq!(r["errors"][0]["line"], json!(1));
    }

    #[test]
    fn multiline_syntax_error_reports_dirty_with_a_line_within_range() {
        // Malformed on line 2 (missing `)` before `{`). Verified dirty against a live Node run;
        // the exact reported line is parser-specific (see module doc), but a 2-line source can
        // only ever report line 1 or 2.
        let r = lint("function f() { return }\nfunction g( { return 1; }");
        assert_eq!(r["clean"], json!(false));
        let line = r["errors"][0]["line"].as_i64().unwrap();
        assert!((1..=2).contains(&line));
    }

    #[test]
    fn transform_message_strips_trailing_loc_suffix() {
        assert_eq!(transform_message("Unexpected token (3:5)"), "Unexpected token");
        assert_eq!(transform_message("Unexpected token  ( 3 : 5 )  "), "Unexpected token  ( 3 : 5 )  ");
        assert_eq!(transform_message("no trailing loc here"), "no trailing loc here");
        assert_eq!(
            transform_message("weird (12:3) but not at the very end more text"),
            "weird (12:3) but not at the very end more text"
        );
        assert_eq!(transform_message("(1:2)"), "");
    }

    #[test]
    fn transform_message_caps_at_200_chars() {
        let long = "a".repeat(250);
        let out = transform_message(&long);
        assert_eq!(out.chars().count(), 200);
        assert_eq!(out, "a".repeat(200));
    }
}
