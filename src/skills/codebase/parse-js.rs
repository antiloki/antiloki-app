//! parse-js.rs — twin of skills/codebase/parse-js.mjs, the JS/JSX language adapter feeding
//! ast-parse.mjs's JS/JSX lane. NAMES ARE CONTRACT: `parseJs` kept exactly as the JS source names
//! it (not snake_cased), matching the parse-treesitter.rs / ts-query.rs precedent.
//!
//! ## Architecture decision (this pass): native `oxc_parser`, not a WASM/JS-only parser
//! JS parses via `acorn` + the `acorn-jsx` plugin (a real npm dependency, not WASM — acorn is a
//! pure-JS recursive-descent parser). Rust has no "just call acorn" option, but it has mature,
//! actively-maintained native ES/JSX parser crates on crates.io. This twin picks `oxc_parser`
//! (the oxc-project's parser, ESTree-shaped AST via `oxc_ast`) over the alternative
//! `swc_ecma_parser` because: (1) oxc is one cohesive, lockstep-versioned project (parser/ast/
//! visitor crates all at 0.143.0), versus swc's many independently-versioned sub-crates; (2)
//! `oxc_ast_visit`'s generated `Visit` trait gives a FULL, already-correct recursive-descent
//! traversal that matches parse-js.mjs's own hand-rolled generic `Object.keys` walk
//! (`collectCalls`/`countDecisions`) node-for-node, with no risk of the port silently skipping a
//! node kind the JS walk would have reached; (3) `SourceType::jsx()` turns JSX on for a plain
//! `.js`/`.mjs`/`.cjs` file exactly like acorn-jsx does — this codebase's JSX-in-.js convention
//! (see the JS source's own top comment about the 2026-07-23 incident) needs JSX ALWAYS on
//! regardless of extension, and oxc supports that directly. `cargo add oxc_parser oxc_ast
//! oxc_span oxc_allocator oxc_ast_visit` resolved cleanly (0.143.0 across all five) against
//! crates.io.
//!
//! ## Necessary Rust-only AST shape differences from acorn's ESTree
//! - **No single `Literal` node.** Acorn/ESTree gives ONE `Literal` type for every literal kind
//!   (string/number/bool/null/regex/bigint) with the concrete value in `.value`. oxc splits each
//!   into its OWN AST type (`StringLiteral`, `NumericLiteral`, `BooleanLiteral`, `NullLiteral`,
//!   `BigIntLiteral`, `RegExpLiteral`) as distinct `Expression` variants. `addConstDecl` in the JS
//!   source only ever needs "is this initializer a literal" (it grabs RAW SOURCE TEXT via
//!   `source.slice(start,end)`, never `.value`), so `is_literal_like` below matches all six
//!   variants plus an empty-interpolation `TemplateLiteral` — mirroring the JS source's own
//!   `d.init.type === 'Literal' || (TemplateLiteral && expressions.length===0)`.
//! - **`MemberExpression` is pre-split by computedness.** Acorn gives one `MemberExpression` node
//!   with a `.computed` boolean; oxc gives `StaticMemberExpression` (non-computed, `.property` is
//!   always an `IdentifierName`) vs `ComputedMemberExpression` (computed) as separate `Expression`
//!   variants. `calleeName`'s acorn check `!callee.computed && callee.property?.type ===
//!   'Identifier'` IS exactly oxc's `Expression::StaticMemberExpression` case — the split IS the
//!   computed flag, so `callee_name` below needs no extra check.
//! - **Rest parameters are not inline in the params array.** Acorn's `fn.params` ends with a
//!   `RestElement` node when the function has one (`paramName` recurses into `.argument` with a
//!   `...` prefix). oxc's `FormalParameters` keeps ordinary params in `.items` and the rest
//!   parameter in a SEPARATE `.rest: Option<Box<FormalParameterRest>>` field. `params_signature`
//!   below reconstructs the acorn-shaped joined list by appending the rest param (with its `...`
//!   prefix) after `.items` — always last, matching JS syntax (a rest param must be final).
//! - **`ExportNamedDeclaration` is split into three oxc node types.** Acorn's single
//!   `ExportNamedDeclaration` covers `export function/const ...` (inline `.declaration`),
//!   `export {a, b};` (bare specifiers, `.declaration` undefined), AND `export {a, b} from 'x';`
//!   (specifiers + source). oxc gives these three shapes their OWN `Statement` variants:
//!   `ExportDeclaration` (has `.declaration: Declaration`), `ExportNamedDeclaration` (specifiers
//!   only, no source), `ExportFromDeclaration` (specifiers + source). The JS source's single
//!   `case 'ExportNamedDeclaration'` switch arm becomes, in this twin, one arm per oxc variant:
//!   `Statement::ExportDeclaration` mirrors the "has declaration" branch; oxc's (specifiers-only)
//!   `Statement::ExportNamedDeclaration` and `Statement::ExportFromDeclaration` mirror the JS
//!   source's implicit `else collectTopLevel(stmt)` branch (declaration is undefined there) —
//!   neither ever contains a `FunctionDeclaration`/`const VariableDeclaration` to add as a node,
//!   so they fall straight to the generic top-level call-walk, exactly like the JS `else` would.
//! - **No generic `Object.keys` walk — `oxc_ast_visit::Visit` instead.** `collectCalls`/
//!   `countDecisions` in the JS source are hand-rolled recursive walks over "every enumerable
//!   child node" (skipping only `loc`/`start`/`end`). oxc's generated `Visit` trait IS that same
//!   full recursive descent, code-generated from the real AST shape — `CallCollector`/
//!   `DecisionCounter` below override only the handful of `visit_*` methods that matter and fall
//!   through to the generated `walk_*` default for everything else.
//!
//! ## Verified vs assumed
//! Cross-checked against a live Node run of the real JS `parseJs` (dynamic `import()`) across 7
//! snippets this pass: a plain function with an `if`/`&&` (complexity + calls), a class with a
//! method calling `draw()` plus a top-level call (no node for the class — matches no switch arm —
//! but `draw()` IS still found via the generic `collectTopLevel` walk, caller=null; note this
//! module has NO class-body special case, unlike parse-treesitter.rs's tsWalk/pyWalk, which
//! deliberately never enters class bodies — this twin's first draft wrongly assumed the
//! tree-sitter twin's behavior applied here too, until the live Node cross-check caught it), a
//! JSX arrow-function component (`export const Comp = () => <div>{items.map(x =>
//! foo(x))}</div>`, confirming JSX-in-.js parses and both the `.map(...)` and nested `foo(...)`
//! calls are found), a plain arrow function assigned to a const (function-node signature +
//! complexity), an import/export mix (`import {a,b} from 'x'` + `export function` + `export
//! const` + bare `export {y}` — confirming a later bare re-export does NOT retroactively mark an
//! earlier plain `const` as exported), a destructured const initializer (`const {x,y} =
//! compute();` — no node, but the call inside the initializer is still walked), and a rest
//! parameter with a non-computed member-expression callee (`obj.reduce(...)`). Every node id/
//! type/name/signature/span/exported flag and every import/call entry matched byte-for-byte
//! between the two lanes, including line numbers (this twin's `line_of` counts `\n` bytes up to a
//! byte offset the same way acorn's `Position.line` counts them for ASCII/BMP source — the shared
//! codebase convention for fixture source, see the playbook).
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, BindingPattern, CallExpression, CatchClause, Class, ConditionalExpression,
    Declaration, DoWhileStatement, ExportDefaultDeclarationKind, Expression, ForInStatement,
    ForOfStatement, ForStatement, FormalParameters, Function, FunctionBody, IfStatement,
    ImportExpression, JSXElement, JSXElementName, JSXFragment, JSXOpeningElement, LogicalExpression,
    Statement, SwitchCase, VariableDeclaration, VariableDeclarationKind, WhileStatement,
};
use oxc_ast_visit::{walk, Visit};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::{GetSpan, SourceType, Span};
use serde_json::{json, Map, Value};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// acorn's `Position.line` is 1-based and counts `\n`s; oxc's `Span` only carries byte offsets, so
// this twin recovers the line number by counting `\n` bytes up to the offset. ASCII/BMP source
// only (byte offset == UTF-16 code unit offset == acorn's offset space) — same assumption the
// playbook's gotcha #5 documents for `.slice`.
fn line_of(source: &str, offset: u32) -> i64 {
    // THE NEWLINE INDEX OF THE SOURCE BEING PARSED (audit 2026-10-05: `line_of` counted every byte up to the offset on every call, so a file with 20 000 top-level consts took 38 s in debug): while `parse_js_as` holds a
    // `LineIndex` for exactly this string (same address and length - the string is borrowed for the whole parse, so nothing else can live there), a line is a binary search; anything else falls back to the count.
    let hit = LINES.with(|c| c.borrow().as_ref().filter(|(p, l, _)| *p == source.as_ptr() as usize && *l == source.len()).map(|(_, _, nl)| 1 + nl.partition_point(|&p| p < offset) as i64));
    hit.unwrap_or_else(|| 1 + source.as_bytes()[..(offset as usize).min(source.len())].iter().filter(|&&b| b == b'\n').count() as i64)
}
thread_local! { static LINES: std::cell::RefCell<Option<(usize, usize, Vec<u32>)>> = const { std::cell::RefCell::new(None) }; }
/// Holds the newline index of one source for the length of a parse; dropping it clears the index.
struct LineIndex;
impl LineIndex {
    fn of(source: &str) -> LineIndex {
        let nl: Vec<u32> = source.as_bytes().iter().enumerate().filter(|(_, &b)| b == b'\n').map(|(i, _)| i as u32).collect();
        LINES.with(|c| *c.borrow_mut() = Some((source.as_ptr() as usize, source.len(), nl)));
        LineIndex
    }
}
impl Drop for LineIndex { fn drop(&mut self) { LINES.with(|c| *c.borrow_mut() = None); } }

fn span_map(path: &str, source: &str, sp: Span) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("file".into(), json!(path));
    m.insert("start_line".into(), json!(line_of(source, sp.start)));
    m.insert("end_line".into(), json!(line_of(source, sp.end)));
    m
}

fn param_name(p: &BindingPattern) -> String {
    match p {
        BindingPattern::BindingIdentifier(id) => id.name.as_str().to_string(),
        BindingPattern::AssignmentPattern(ap) => param_name(&ap.left),
        BindingPattern::ObjectPattern(_) => "{…}".to_string(),
        BindingPattern::ArrayPattern(_) => "[…]".to_string(),
    }
}

// See module doc: oxc keeps a rest param OUT of `.items`, in a separate `.rest` field — appended
// last here to reconstruct the acorn-shaped joined param list (`paramName` mapped over `fn.params`).
fn params_signature(params: &FormalParameters) -> String {
    let mut names: Vec<String> = params.items.iter().map(|p| param_name(&p.pattern)).collect();
    if let Some(rest) = &params.rest {
        names.push(format!("...{}", param_name(&rest.rest.argument)));
    }
    format!("({})", names.join(", "))
}

// See module doc: acorn's single `Literal` type, split across 6 oxc `Expression` variants + the
// empty-interpolation `TemplateLiteral` case.
fn is_literal_like(e: &Expression) -> bool {
    matches!(
        e,
        Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::RegExpLiteral(_)
            | Expression::StringLiteral(_)
    ) || matches!(e, Expression::TemplateLiteral(t) if t.expressions.is_empty())
}

// The bare callee NAME of a call: `foo(…)` -> "foo"; `a.b(…)` -> "b" (the property); anything
// else (computed member, IIFE, optional-chain member on a non-identifier, …) -> None = skip. See
// module doc: oxc's `StaticMemberExpression` IS the "non-computed member with Identifier
// property" case, no extra `computed` check needed.
fn callee_name(callee: &Expression) -> Option<String> {
    match callee {
        Expression::Identifier(id) => Some(id.name.as_str().to_string()),
        Expression::StaticMemberExpression(sme) => Some(sme.property.name.as_str().to_string()),
        _ => None,
    }
}

// ── collectCalls: full recursive descent collecting every CallExpression callee name + span ──
// Also collects JSX component usages (`<Row/>`, `<ui.Button/>`) as `renders` — the React way one
// component "calls" another. Host elements (`<div/>`) are not components and are skipped.
#[derive(Default)]
struct CallCollector {
    calls: Vec<(String, Span)>,
    renders: Vec<(String, Span)>,
    refs: Vec<(String, Span)>,
}
impl<'a> Visit<'a> for CallCollector {
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Some(name) = callee_name(&it.callee) {
            self.calls.push((name, it.span));
        }
        // bare identifiers handed to the call — the ones naming a definition become `uses` edges
        for a in &it.arguments {
            if let Some(Expression::Identifier(id)) = a.as_expression() {
                self.refs.push((id.name.as_str().to_string(), id.span));
            }
        }
        walk::walk_call_expression(self, it);
    }
    fn visit_new_expression(&mut self, it: &oxc_ast::ast::NewExpression<'a>) {
        // `new X()` — constructing a class is calling it (see parse-treesitter.rs)
        if let Some(name) = callee_name(&it.callee) {
            self.calls.push((name, it.span));
        }
        walk::walk_new_expression(self, it);
    }
    fn visit_jsx_opening_element(&mut self, it: &JSXOpeningElement<'a>) {
        let name = match &it.name {
            JSXElementName::Identifier(id) => Some(id.name.as_str().to_string()),
            JSXElementName::IdentifierReference(id) => Some(id.name.as_str().to_string()),
            JSXElementName::MemberExpression(m) => Some(m.property.name.as_str().to_string()),
            _ => None,
        };
        if let Some(n) = name {
            if crate::codebase_parse_treesitter::is_pascal_case(&n) {
                self.renders.push((n, it.span));
            }
        }
        walk::walk_jsx_opening_element(self, it);
    }
}

// Does this subtree render JSX? The component test: a PascalCase function whose body contains
// an element or a fragment.
#[derive(Default)]
struct JsxFinder {
    found: bool,
}
impl<'a> Visit<'a> for JsxFinder {
    fn visit_jsx_element(&mut self, _it: &JSXElement<'a>) {
        self.found = true;
    }
    fn visit_jsx_fragment(&mut self, _it: &JSXFragment<'a>) {
        self.found = true;
    }
}

// `import('./x')` anywhere in the file (oxc: `Expression::ImportExpression`, NOT a call — the
// call collector never sees it).
#[derive(Default)]
struct DynamicImportCollector {
    found: Vec<(String, Span)>,
}
impl<'a> Visit<'a> for DynamicImportCollector {
    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if let Expression::StringLiteral(s) = &it.source {
            self.found.push((s.value.as_str().to_string(), it.span));
        }
        walk::walk_import_expression(self, it);
    }
}

fn is_hook_name(name: &str) -> bool {
    name.len() > 3 && name.starts_with("use") && name[3..].chars().next().map_or(false, |c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Same rule as parse-treesitter.rs's `ts_def_kind`: `useX` → hook; PascalCase + renders JSX →
/// component; else function. `has_jsx` is computed by the caller over the body it has.
fn def_kind(name: &str, has_jsx: bool) -> &'static str {
    if is_hook_name(name) {
        "hook"
    } else if crate::codebase_parse_treesitter::is_pascal_case(name) && has_jsx {
        "component"
    } else {
        "function"
    }
}

const WRAPPERS: &[&str] = &["memo", "forwardRef", "observer"];

/// `memo(inner)` / `React.forwardRef(inner)` → inner, for the wrapper set.
fn wrapped_arg<'a, 'b>(call: &'b CallExpression<'a>) -> Option<&'b Expression<'a>> {
    let callee = callee_name(&call.callee)?;
    if !WRAPPERS.contains(&callee.as_str()) {
        return None;
    }
    call.arguments.first().and_then(|a| a.as_expression())
}

fn class_kind(c: &Class) -> &'static str {
    let is_react = match &c.super_class {
        Some(Expression::Identifier(id)) => id.name.as_str().contains("Component"),
        Some(Expression::StaticMemberExpression(m)) => m.property.name.as_str().contains("Component"),
        _ => false,
    };
    if is_react { "component" } else { "class" }
}

// ── countDecisions: 1 + decision points, full recursive descent ──
#[derive(Default)]
struct DecisionCounter {
    n: i64,
}
impl<'a> Visit<'a> for DecisionCounter {
    fn visit_if_statement(&mut self, it: &IfStatement<'a>) {
        self.n += 1;
        walk::walk_if_statement(self, it);
    }
    fn visit_for_statement(&mut self, it: &ForStatement<'a>) {
        self.n += 1;
        walk::walk_for_statement(self, it);
    }
    fn visit_for_in_statement(&mut self, it: &ForInStatement<'a>) {
        self.n += 1;
        walk::walk_for_in_statement(self, it);
    }
    fn visit_for_of_statement(&mut self, it: &ForOfStatement<'a>) {
        self.n += 1;
        walk::walk_for_of_statement(self, it);
    }
    fn visit_while_statement(&mut self, it: &WhileStatement<'a>) {
        self.n += 1;
        walk::walk_while_statement(self, it);
    }
    fn visit_do_while_statement(&mut self, it: &DoWhileStatement<'a>) {
        self.n += 1;
        walk::walk_do_while_statement(self, it);
    }
    fn visit_catch_clause(&mut self, it: &CatchClause<'a>) {
        self.n += 1;
        walk::walk_catch_clause(self, it);
    }
    fn visit_conditional_expression(&mut self, it: &ConditionalExpression<'a>) {
        self.n += 1;
        walk::walk_conditional_expression(self, it);
    }
    fn visit_switch_case(&mut self, it: &SwitchCase<'a>) {
        if it.test.is_some() {
            self.n += 1;
        }
        walk::walk_switch_case(self, it);
    }
    fn visit_logical_expression(&mut self, it: &LogicalExpression<'a>) {
        // every LogicalOperator variant (&&, ||, ??) counts — matches the JS source's operator check
        self.n += 1;
        walk::walk_logical_expression(self, it);
    }
}

struct Out {
    nodes: Vec<Value>,
    imports: Vec<Value>,
    calls: Vec<Value>,
    refs: Vec<Value>, // definitions passed by name — see parse-treesitter.rs `Out::refs`
}

fn push_calls(out: &mut Out, path: &str, source: &str, caller: Option<&str>, cc: CallCollector) {
    for (name, sp) in cc.calls {
        out.calls.push(json!({
            "caller": caller,
            "callee": name,
            "span": Value::Object(span_map(path, source, sp)),
        }));
    }
    // one `renders` edge per (caller, child) — a list renders the same row N times, that is one edge
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, sp) in cc.renders {
        if seen.insert(name.clone()) {
            out.calls.push(json!({
                "caller": caller,
                "callee": name,
                "via": "jsx",
                "span": Value::Object(span_map(path, source, sp)),
            }));
        }
    }
    let mut seen_ref: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, sp) in cc.refs {
        if seen_ref.insert(name.clone()) {
            out.refs.push(json!({
                "caller": caller,
                "callee": name,
                "span": Value::Object(span_map(path, source, sp)),
            }));
        }
    }
}

// collectTopLevel(stmt): calls sitting outside any top-level fn -> caller=null.
fn collect_top_level_stmt(out: &mut Out, path: &str, source: &str, stmt: &Statement) {
    let mut cc = CallCollector::default();
    cc.visit_statement(stmt);
    push_calls(out, path, source, None, cc);
}

// collectTopLevel(d.init) — the JS source also calls it directly on an Expression.
fn collect_top_level_expr(out: &mut Out, path: &str, source: &str, expr: &Expression) {
    let mut cc = CallCollector::default();
    cc.visit_expression(expr);
    push_calls(out, path, source, None, cc);
}

// Shared by addFn (FunctionDeclaration) and the FunctionExpression-as-const-initializer case in
// addConstDecl — both wrap the SAME oxc `Function` struct, differing only in which span anchors
// the emitted node (the function's own span vs. the enclosing VariableDeclarator's span).
#[allow(clippy::too_many_arguments)]
fn add_function_node(
    out: &mut Out,
    path: &str,
    source: &str,
    name: &str,
    node_span: Span,
    params: &FormalParameters,
    body: Option<&FunctionBody>,
    exported: bool,
    force_kind: Option<&'static str>,
) {
    let mut dc = DecisionCounter::default();
    let mut jf = JsxFinder::default();
    if let Some(b) = body {
        dc.visit_function_body(b);
        jf.visit_function_body(b);
    }
    let kind = force_kind.unwrap_or_else(|| def_kind(name, jf.found));
    let mut sp = span_map(path, source, node_span);
    sp.insert("complexity".into(), json!(1 + dc.n));
    out.nodes.push(json!({
        "id": format!("{path}#{name}"),
        "path": path,
        "type": kind,
        "name": name,
        "signature": params_signature(params),
        "parent_id": path,
        "span": Value::Object(sp),
        "exported": exported,
    }));
    let mut cc = CallCollector::default();
    if let Some(b) = body {
        cc.visit_function_body(b);
    }
    push_calls(out, path, source, Some(name), cc);
}

#[allow(non_snake_case)]
fn addFn(out: &mut Out, path: &str, source: &str, f: &Function, exported: bool) {
    let Some(id) = &f.id else { return };
    let name = id.name.as_str().to_string();
    add_function_node(out, path, source, &name, f.span, &f.params, f.body.as_deref(), exported, None);
}

// A class is a node: `component` when it extends (React.)Component/PureComponent, else `class`.
// The old "no node for a class" behaviour was parity with the deleted JS lane; the atlas's own
// vocabulary has always had `class`, and a React class component is as much a component as a
// function one.
fn add_class_node(out: &mut Out, path: &str, source: &str, c: &Class, exported: bool) {
    let Some(id) = &c.id else { return };
    let name = id.name.as_str().to_string();
    out.nodes.push(json!({
        "id": format!("{path}#{name}"),
        "path": path,
        "type": class_kind(c),
        "name": name,
        "signature": "()",
        "parent_id": path,
        "span": Value::Object(span_map(path, source, c.span)),
        "exported": exported,
    }));
    let mut cc = CallCollector::default();
    cc.visit_class(c);
    push_calls(out, path, source, Some(&name), cc);
}

fn push_plain_node(out: &mut Out, path: &str, source: &str, kind: &str, name: &str, signature: String, node_span: Span, exported: bool) {
    out.nodes.push(json!({
        "id": format!("{path}#{name}"),
        "path": path,
        "type": kind,
        "name": name,
        "signature": signature,
        "parent_id": path,
        "span": Value::Object(span_map(path, source, node_span)),
        "exported": exported,
    }));
}

// FEAT-DESIGN-BINDS-IDENTIFIERS: top-level const declarations are real interface surface (see
// the JS source's own comment). arrow/function-expression initializer -> a 'function' node;
// literal initializer -> a 'const' node (signature = the raw source text); anything else (styled
// components, object literals, …) -> no node, calls still collected.
#[allow(non_snake_case)]
fn addConstDecl(out: &mut Out, path: &str, source: &str, stmt: &VariableDeclaration, exported: bool) {
    for d in &stmt.declarations {
        let BindingPattern::BindingIdentifier(id) = &d.id else {
            if let Some(init) = &d.init {
                collect_top_level_expr(out, path, source, init);
            }
            continue;
        };
        let Some(init) = &d.init else { continue };
        let name = id.name.as_str().to_string();
        let src_text = |e: &Expression| -> String {
            let sp = e.span();
            source[sp.start as usize..sp.end as usize].chars().take(120).collect()
        };
        match init {
            Expression::ArrowFunctionExpression(af) => {
                add_arrow_node(out, path, source, &name, d.span, af, exported, None);
            }
            Expression::FunctionExpression(fe) => {
                add_function_node(out, path, source, &name, d.span, &fe.params, fe.body.as_deref(), exported, None);
            }
            // `forwardRef((props, ref) => …)` / `memo(function Icon() {…})` — the wrapped fn is the
            // component; `memo(Inner)` is a component alias with nothing of its own to score.
            Expression::CallExpression(call) if wrapped_arg(call).is_some() => {
                let pascal = crate::codebase_parse_treesitter::is_pascal_case(&name);
                match wrapped_arg(call).unwrap() {
                    Expression::ArrowFunctionExpression(af) => {
                        add_arrow_node(out, path, source, &name, d.span, af, exported, if pascal { Some("component") } else { None });
                    }
                    Expression::FunctionExpression(fe) => {
                        add_function_node(out, path, source, &name, d.span, &fe.params, fe.body.as_deref(), exported, if pascal { Some("component") } else { None });
                    }
                    Expression::Identifier(_) if pascal => {
                        push_plain_node(out, path, source, "component", &name, format!("= {}", src_text(init)), d.span, exported);
                    }
                    _ => collect_top_level_expr(out, path, source, init),
                }
            }
            // `export const useStore = StoreContext.useContext` — a hook alias
            Expression::StaticMemberExpression(m) if is_hook_name(&name) && m.property.name.as_str().starts_with("use") => {
                push_plain_node(out, path, source, "hook", &name, format!("= {}", src_text(init)), d.span, exported);
            }
            _ if is_literal_like(init) => {
                let start = init.span().start as usize;
                let end = init.span().end as usize;
                let text: String = source[start..end].chars().take(120).collect();
                out.nodes.push(json!({
                    "id": format!("{path}#{name}"),
                    "path": path,
                    "type": "const",
                    "name": name,
                    "signature": format!("= {text}"),
                    "parent_id": path,
                    "span": Value::Object(span_map(path, source, d.span)),
                    "exported": exported,
                }));
            }
            _ => collect_top_level_expr(out, path, source, init),
        }
    }
}

// Arrow functions have a distinct body shape (`ArrowFunctionBody`: a `FunctionBody` OR a bare
// concise-arrow `Expression`) so this stays separate from add_function_node rather than forcing a
// shared abstraction over two genuinely different body types.
#[allow(clippy::too_many_arguments)]
fn add_arrow_node(
    out: &mut Out,
    path: &str,
    source: &str,
    name: &str,
    node_span: Span,
    af: &ArrowFunctionExpression,
    exported: bool,
    force_kind: Option<&'static str>,
) {
    let mut dc = DecisionCounter::default();
    dc.visit_arrow_function_body(&af.body);
    let mut jf = JsxFinder::default();
    jf.visit_arrow_function_body(&af.body);
    let kind = force_kind.unwrap_or_else(|| def_kind(name, jf.found));
    let mut sp = span_map(path, source, node_span);
    sp.insert("complexity".into(), json!(1 + dc.n));
    out.nodes.push(json!({
        "id": format!("{path}#{name}"),
        "path": path,
        "type": kind,
        "name": name,
        "signature": params_signature(&af.params),
        "parent_id": path,
        "span": Value::Object(sp),
        "exported": exported,
    }));
    let mut cc = CallCollector::default();
    cc.visit_arrow_function_body(&af.body);
    push_calls(out, path, source, Some(name), cc);
}

/// parseJs(source, path) — no-db helper, 2 positional args. See module doc for the architecture
/// decision (native `oxc_parser`) and the AST-shape differences this twin bridges.
#[allow(non_snake_case)]
pub fn parseJs(source: &Value, path: &Value) -> Value {
    let (mut v, status) = parse_js_as(&str_qq(Some(source)), &str_qq(Some(path)), false);
    // audit 2026-10-01, F-06: oxc gives up on some input (`panicked`) and hands back an EMPTY program; this twin returns only the Value, so the failure is written INTO it
    // (`parse_failed`, plus `has_error`) rather than dropped - a failed parse must not read as a clean file with no symbols. `parseJsChecked` is the Result form.
    if let (ParseStatus::Panicked(msg), Some(o)) = (&status, v.as_object_mut()) {
        o.insert("has_error".into(), json!(true));
        o.insert("parse_failed".into(), json!(true));
        o.insert("first_error".into(), json!(msg));
    }
    v
}

/// `parseJs` as a Result: `Err` when oxc gave up on the file (`panicked`), the plain lane's counterpart of `parseJsFlow`'s failure. `ast-parse` uses this one, so a file the parser
/// could not read is recorded as a failed parse and not as a clean file with no symbols (audit 2026-10-01, F-06: the plain lane threw `panicked` away).
#[allow(non_snake_case)]
pub fn parseJsChecked(source: &str, path: &str) -> Result<Value, String> {
    match parse_js_as(source, path, false) {
        (_, ParseStatus::Panicked(msg)) => Err(format!("parse: the parser gave up on this file — {msg}")),
        (v, _) => Ok(v),
    }
}

/// Flow-typed JavaScript (`@flow`, `import type`, `(x: T)` casts …): the source is run through
/// `flow_strip::stripFlow` and parsed with oxc's TSX grammar. Returns the same shape as `parseJs`
/// plus `has_error` (oxc's own error list was not empty — a type construct the pre-pass does not
/// know) — or `Err` when oxc gave up on the file (`panicked`: it returns an EMPTY program then, which
/// must never be mistaken for a file with no symbols).
#[allow(non_snake_case)]
pub fn parseJsFlow(source: &str, path: &str) -> Result<Value, String> {
    let (stripped, budget) = crate::codebase_flow_strip::stripFlowChecked(source);
    // the pre-pass gave up (too big, absurdly nested, or a scan that would not end): the file comes back unstripped, and reading Flow as TypeScript would only pile up errors
    if budget {
        return Err("flow: the pre-pass gave up (the file is too large, nests absurdly deep, or would have taken too long) — left unparsed".to_string());
    }
    let (mut v, mut status) = parse_js_as(&stripped, path, true);
    // a bare `>` or `}` in JSX text is Flow-legal and oxc-illegal: swap it for a space where oxc names it and read the file again (flow_strip::repair_jsx_text)
    if matches!(&status, ParseStatus::Panicked(m) | ParseStatus::Errors(_, m) if m.contains("Did you mean `{'")) {
        (v, status) = parse_js_as(&crate::codebase_flow_strip::repair_jsx_text(&stripped), path, true);
    }
    match status {
        ParseStatus::Panicked(msg) => Err(format!("flow: parser gave up after type erasure — {msg}")),
        ParseStatus::Errors(n, first) => {
            if let Some(o) = v.as_object_mut() {
                o.insert("has_error".into(), json!(true));
                o.insert("error_count".into(), json!(n));
                o.insert("first_error".into(), json!(first));
            }
            Ok(v)
        }
        ParseStatus::Clean => Ok(v),
    }
}

/// How many of oxc's diagnostics are given a line number (the first is what the health row shows).
const MAX_DIAGNOSTIC_LINES: usize = 50;

enum ParseStatus {
    Clean,
    /// (count, the first message) — oxc recovered and produced a program
    Errors(usize, String),
    Panicked(String),
}

fn parse_js_as(source_s: &str, path_s: &str, flow: bool) -> (Value, ParseStatus) {
    let _lines = LineIndex::of(source_s);
    let allocator = Allocator::default();
    // SourceType::jsx() = JavaScript + Module + JSX — matches acorn's `sourceType:'module'` plus
    // the acorn-jsx plugin, applied unconditionally regardless of the actual file extension (see
    // module doc: JSX-in-.js is always parsed, mirroring the JS source's module-level JsParser).
    // Flow files run through `stripFlow` first and are read as TSX (annotations, `import type`, generics).
    let source_type = if flow { SourceType::tsx() } else { SourceType::jsx() };
    // preserve_parens:false — acorn/ESTree never wraps a parenthesized expression in its own
    // node; oxc defaults to `true` (a non-standard `ParenthesizedExpression` wrapper), which would
    // make `const x = (foo());` fail to match the ArrowFunctionExpression/FunctionExpression/
    // literal checks below. Disabling it keeps the AST directly ESTree-shaped for this walk.
    let ret = Parser::new(&allocator, &source_s, source_type)
        .with_options(ParseOptions { preserve_parens: false, ..ParseOptions::default() })
        .parse();
    // the first error carries its line: the health row is where the next fix to the Flow pre-pass starts
    // …but only the first MAX_DIAGNOSTIC_LINES are given a line number: `line_of` counts newlines from the start of the file, and a hostile file can carry one diagnostic per byte
    // (audit 2026-10-01, F-06: diagnostics × bytes). The count is still the real total.
    let n_diag = ret.diagnostics.errors().count();
    let msgs: Vec<String> = ret.diagnostics.errors().take(MAX_DIAGNOSTIC_LINES).map(|d| match d.labels.as_slice().first() {
        Some(l) => format!("{} (line {})", d.message, line_of(source_s, (l.offset() as u32).min(source_s.len() as u32))),
        None => d.message.to_string(),
    }).collect();
    let status = if ret.panicked {
        ParseStatus::Panicked(msgs.first().cloned().unwrap_or_default())
    } else if flow && n_diag > 0 {
        ParseStatus::Errors(n_diag, msgs[0].clone())
    } else {
        ParseStatus::Clean
    };
    let program = ret.program;

    let mut out = Out {
        nodes: vec![crate::codebase_parse_treesitter::file_node(&path_s, &source_s)],
        imports: Vec::new(),
        calls: Vec::new(),
        refs: Vec::new(),
    };

    let push_import = |out: &mut Out, specifier: &str, names: Vec<Value>, kind: &str, sp: Span| {
        out.imports.push(json!({
            "specifier": specifier,
            "names": names,
            "kind": kind,
            "span": Value::Object(span_map(&path_s, &source_s, sp)),
        }));
    };

    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(imp) => {
                let specifier = imp.source.value.as_str().to_string();
                let names: Vec<Value> = imp
                    .specifiers
                    .as_ref()
                    .map(|specs| specs.iter().map(|s| json!(s.local().name.as_str())).collect())
                    .unwrap_or_default();
                // `import type {X}` or every specifier `type` — erased at compile time
                let all_specs_type = imp.specifiers.as_ref().map_or(false, |specs| {
                    !specs.is_empty() && specs.iter().all(|s| matches!(s, oxc_ast::ast::ImportDeclarationSpecifier::ImportSpecifier(is) if is.import_kind.is_type()))
                });
                let kind = if imp.import_kind.is_type() || all_specs_type { "type" } else { "value" };
                push_import(&mut out, &specifier, names, kind, imp.span);
            }
            // `export {a} from './x'` / `export * from './x'` — barrel edges
            Statement::ExportFromDeclaration(efd) => {
                let kind = if efd.export_kind.is_type() { "type" } else { "reexport" };
                push_import(&mut out, efd.source.value.as_str(), vec![], kind, efd.span);
            }
            Statement::ExportAllDeclaration(ead) => {
                let kind = if ead.export_kind.is_type() { "type" } else { "reexport" };
                push_import(&mut out, ead.source.value.as_str(), vec![], kind, ead.span);
            }
            Statement::FunctionDeclaration(f) => {
                addFn(&mut out, &path_s, &source_s, f, false);
            }
            Statement::ClassDeclaration(c) => {
                add_class_node(&mut out, &path_s, &source_s, c, false);
            }
            Statement::VariableDeclaration(vd) => {
                if matches!(vd.kind, VariableDeclarationKind::Const) {
                    addConstDecl(&mut out, &path_s, &source_s, vd, false);
                } else {
                    collect_top_level_stmt(&mut out, &path_s, &source_s, stmt);
                }
            }
            // acorn's ExportNamedDeclaration WITH an inline declaration — see module doc.
            Statement::ExportDeclaration(ed) => match &ed.declaration {
                Declaration::FunctionDeclaration(f) => addFn(&mut out, &path_s, &source_s, f, true),
                Declaration::ClassDeclaration(c) => add_class_node(&mut out, &path_s, &source_s, c, true),
                Declaration::VariableDeclaration(vd) if matches!(vd.kind, VariableDeclarationKind::Const) => {
                    addConstDecl(&mut out, &path_s, &source_s, vd, true);
                }
                _ => collect_top_level_stmt(&mut out, &path_s, &source_s, stmt),
            },
            Statement::ExportDefaultDeclaration(edd) => match &edd.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    addFn(&mut out, &path_s, &source_s, f, true);
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    add_class_node(&mut out, &path_s, &source_s, c, true);
                }
                _ => collect_top_level_stmt(&mut out, &path_s, &source_s, stmt),
            },
            // covers acorn's bare `export {a,b};` / `export {a,b} from 'x';` shapes too (oxc's
            // ExportNamedDeclaration/ExportFromDeclaration) — see module doc.
            _ => collect_top_level_stmt(&mut out, &path_s, &source_s, stmt),
        }
    }

    // lazy seams live inside function bodies — one whole-program pass
    let mut dyn_imports = DynamicImportCollector::default();
    dyn_imports.visit_program(&program);
    for (spec, sp) in dyn_imports.found {
        push_import(&mut out, &spec, vec![], "dynamic", sp);
    }

    let value = json!({
        "nodes": out.nodes,
        "imports": out.imports,
        "calls": out.calls,
        "refs": out.refs,
    });
    (value, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        json!(v)
    }

    #[test]
    fn plain_function_with_if_and_logical_and() {
        let source = "function add(a, b) {\n  if (a > 0 && b > 0) {\n    return helper(a, b);\n  }\n  return b;\n}\n";
        let result = parseJs(&s(source), &s("foo.js"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2); // file + add
        assert_eq!(nodes[0]["type"], json!("file"));
        assert_eq!(nodes[0]["span"]["start_line"], json!(1), "the file node carries its own span so loc is a measurement");
        assert_eq!(nodes[1]["id"], json!("foo.js#add"));
        assert_eq!(nodes[1]["name"], json!("add"));
        assert_eq!(nodes[1]["type"], json!("function"));
        assert_eq!(nodes[1]["signature"], json!("(a, b)"));
        assert_eq!(nodes[1]["exported"], json!(false));
        assert_eq!(nodes[1]["span"]["complexity"], json!(3)); // base + if + &&
        assert_eq!(nodes[1]["span"]["start_line"], json!(1));
        assert_eq!(nodes[1]["span"]["end_line"], json!(6));

        let calls = result["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], json!({ "caller": "add", "callee": "helper", "span": { "file": "foo.js", "start_line": 3, "end_line": 3 } }));
        assert!(result["imports"].as_array().unwrap().is_empty());
    }

    #[test]
    fn class_declaration_is_a_class_node_and_calls_are_attributed_to_it() {
        // Until 2026-09-12 a class produced NO node (parity with the deleted JS lane, whose
        // `collectTopLevel` walk never made one). The atlas vocabulary always had `class`; a
        // React class component is a `component`. Calls inside the body belong to the class.
        let source = "class Widget {\n  render() {\n    return draw();\n  }\n}\n\ntopLevelCall(1, 2);\n";
        let result = parseJs(&s(source), &s("widget.js"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1]["type"], json!("class"));
        assert_eq!(nodes[1]["name"], json!("Widget"));

        let calls = result["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], json!({ "caller": "Widget", "callee": "draw", "span": { "file": "widget.js", "start_line": 3, "end_line": 3 } }));
        assert_eq!(calls[1], json!({ "caller": null, "callee": "topLevelCall", "span": { "file": "widget.js", "start_line": 7, "end_line": 7 } }));

        let react = parseJs(&s("class Panel extends React.Component { render() { return <p/>; } }\n"), &s("p.js"));
        assert_eq!(react["nodes"][1]["type"], json!("component"));
    }

    #[test]
    fn jsx_arrow_component_with_nested_calls() {
        let source = "export const Comp = (items) => {\n  return <div>{items.map(x => foo(x))}</div>;\n};\n";
        let result = parseJs(&s(source), &s("comp.js"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1]["name"], json!("Comp"));
        assert_eq!(nodes[1]["type"], json!("component"), "PascalCase + renders JSX");
        assert_eq!(nodes[1]["signature"], json!("(items)"));
        assert_eq!(nodes[1]["exported"], json!(true));
        assert_eq!(nodes[1]["span"]["complexity"], json!(1));

        let calls = result["calls"].as_array().unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c["callee"].as_str().unwrap()).collect();
        assert!(callees.contains(&"map"));
        assert!(callees.contains(&"foo"));
        assert!(calls.iter().all(|c| c["caller"] == json!("Comp")));
    }

    #[test]
    fn plain_arrow_const_function_and_complexity() {
        let source = "const mul = (a, b) => {\n  for (let i = 0; i < a; i++) {\n    log(i);\n  }\n  return a * b;\n};\n";
        let result = parseJs(&s(source), &s("mul.js"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1]["name"], json!("mul"));
        assert_eq!(nodes[1]["type"], json!("function"));
        assert_eq!(nodes[1]["exported"], json!(false));
        assert_eq!(nodes[1]["span"]["complexity"], json!(2)); // base + for

        let calls = result["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], json!({ "caller": "mul", "callee": "log", "span": { "file": "mul.js", "start_line": 3, "end_line": 3 } }));
    }

    #[test]
    fn import_export_mix_and_bare_named_export() {
        let source = "import { a, b } from 'lib';\n\nexport function greet(name) {\n  return a(name);\n}\n\nexport const STORAGE_KEY = 'x';\n\nconst z = 1;\n\nexport { z };\n";
        let result = parseJs(&s(source), &s("mix.js"));

        let imports = result["imports"].as_array().unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0]["specifier"], json!("lib"));
        assert_eq!(imports[0]["names"], json!(["a", "b"]));
        assert_eq!(imports[0]["span"]["start_line"], json!(1));

        let nodes = result["nodes"].as_array().unwrap();
        // file + greet (function) + STORAGE_KEY (const) + z (const)
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[1]["name"], json!("greet"));
        assert_eq!(nodes[1]["type"], json!("function"));
        assert_eq!(nodes[1]["exported"], json!(true));
        assert_eq!(nodes[2]["name"], json!("STORAGE_KEY"));
        assert_eq!(nodes[2]["type"], json!("const"));
        assert_eq!(nodes[2]["signature"], json!("= 'x'"));
        assert_eq!(nodes[2]["exported"], json!(true));
        assert_eq!(nodes[3]["name"], json!("z"));
        assert_eq!(nodes[3]["type"], json!("const"));
        assert_eq!(nodes[3]["exported"], json!(false)); // `const z = 1;` itself isn't exported — the later bare `export { z };` doesn't retroactively mark it

        let calls = result["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], json!({ "caller": "greet", "callee": "a", "span": { "file": "mix.js", "start_line": 4, "end_line": 4 } }));
    }

    #[test]
    fn destructured_const_initializer_walked_as_top_level_call() {
        let source = "const { x, y } = compute();\n";
        let result = parseJs(&s(source), &s("destr.js"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 1); // only the file node — destructured declarators aren't extracted

        let calls = result["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], json!({ "caller": null, "callee": "compute", "span": { "file": "destr.js", "start_line": 1, "end_line": 1 } }));
    }

    #[test]
    fn rest_param_and_member_callee() {
        let source = "function sum(a, ...rest) {\n  return obj.reduce(a, rest);\n}\n";
        let result = parseJs(&s(source), &s("sum.js"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes[1]["signature"], json!("(a, ...rest)"));

        let calls = result["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["callee"], json!("reduce")); // obj.reduce(...) -> property name, not "obj"
    }
}

// ── React reading (2026-09-12) on the oxc lane — the same rule as parse-treesitter.rs ──
#[cfg(test)]
mod react_tests {
    use super::*;

    const SRC: &str = r#"import {memo, forwardRef} from 'react';
export {default as Editor} from './Editor';
export * from './all';
const Lazy = dynamic(() => import('./Impl'), {ssr: false});
export default function Header() { return <div><Row a={1}/><Row a={2}/><ui.Button/></div>; }
const Message = forwardRef((props, ref) => <div ref={ref}/>);
export const Icon = memo(function Icon(props) { return <svg/>; });
export const useStore = Ctx.useContext;
const Wrapped = memo(Inner);
export const Styled = styled.div`color: red`;
class Store extends React.Component { render() { return <p/>; } }
export const CAP = 10;
function useThing() { if (x) { return 1; } return 2; }
function helper(a) { return a; }
"#;

    fn node(v: &Value, name: &str) -> Value {
        v["nodes"].as_array().unwrap().iter().find(|n| n["name"] == json!(name)).cloned().unwrap_or_else(|| panic!("no def {name}: {:?}", v["nodes"].as_array().unwrap().iter().map(|n| n["name"].clone()).collect::<Vec<_>>()))
    }

    #[test]
    fn constructing_a_class_is_a_call() {
        let v = parseJs(&json!("export function run(fn) { visit(fn, new Transform(), false); }\nclass Transform extends Base {}\n"), &json!("p.js"));
        let calls: Vec<(&str, &str)> = v["calls"].as_array().unwrap().iter().map(|c| (c["caller"].as_str().unwrap(), c["callee"].as_str().unwrap())).collect();
        assert!(calls.contains(&("run", "Transform")), "{calls:?}");
    }

    #[test]
    fn components_hooks_wrappers_imports_and_renders() {
        let v = parseJs(&json!(SRC), &json!("a.js"));
        assert_eq!(node(&v, "Header")["type"], json!("component"));
        assert_eq!(node(&v, "Message")["type"], json!("component"));
        assert_eq!(node(&v, "Message")["signature"], json!("(props, ref)"));
        assert_eq!(node(&v, "Icon")["type"], json!("component"));
        assert_eq!(node(&v, "useStore")["type"], json!("hook"));
        assert_eq!(node(&v, "useStore")["signature"], json!("= Ctx.useContext"));
        assert_eq!(node(&v, "Wrapped")["type"], json!("component"));
        assert!(node(&v, "Wrapped")["span"].get("complexity").is_none());
        assert_eq!(node(&v, "Store")["type"], json!("component"));
        assert_eq!(node(&v, "useThing")["type"], json!("hook"));
        assert_eq!(node(&v, "useThing")["span"]["complexity"], json!(2));
        assert_eq!(node(&v, "helper")["type"], json!("function"));
        assert_eq!(node(&v, "CAP")["type"], json!("const"));
        assert!(!v["nodes"].as_array().unwrap().iter().any(|n| n["name"] == json!("Styled") || n["name"] == json!("Lazy")));

        let imports: Vec<(String, String)> = v["imports"].as_array().unwrap().iter().map(|i| (i["specifier"].as_str().unwrap().to_string(), i["kind"].as_str().unwrap().to_string())).collect();
        let expect: Vec<(String, String)> = [("react", "value"), ("./Editor", "reexport"), ("./all", "reexport"), ("./Impl", "dynamic")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        assert_eq!(imports, expect);

        let renders: Vec<(String, String)> = v["calls"].as_array().unwrap().iter().filter(|c| c["via"] == json!("jsx")).map(|c| (c["caller"].as_str().unwrap().to_string(), c["callee"].as_str().unwrap().to_string())).collect();
        assert_eq!(renders, vec![("Header".to_string(), "Row".to_string()), ("Header".to_string(), "Button".to_string())]);
        assert_eq!(v["nodes"][0]["span"]["end_line"], json!(SRC.lines().count()));
    }

    // F-06 (audit 2026-10-01): oxc's `panicked` (an unrecoverable syntax error: it returns an EMPTY program) was thrown away by the plain lane, so a file the parser could not read
    // was recorded as a clean file with no symbols.
    #[test]
    fn a_plain_file_oxc_gave_up_on_is_a_failed_parse_not_a_clean_empty_one() {
        for broken in ["}\n", "function (", "export function f( {\n"] {
            let e = parseJsChecked(broken, "a.js").unwrap_err();
            assert!(e.contains("gave up"), "{broken:?}: {e}");
            let v = parseJs(&json!(broken), &json!("a.js"));
            assert_eq!(v["parse_failed"], json!(true), "the Value form says so too: {v}");
            assert_eq!(v["has_error"], json!(true));
        }
        let ok = parseJsChecked("export function f() { return 1; }\n", "a.js").unwrap();
        assert!(ok.get("parse_failed").is_none() && ok["nodes"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn only_the_first_diagnostics_are_given_a_line_number_and_the_count_stays_true() {
        // 60 000 recoverable errors (a top-level `return`) and no nodes: the line lookup is a newline count from the top of the file, so it must not run once per diagnostic
        let src = format!("// @flow\n{}", "return 1;\n".repeat(60_000));
        let t0 = std::time::Instant::now();
        let v = parseJsFlow(&src, "x.js").unwrap();
        assert!(t0.elapsed() < std::time::Duration::from_secs(8), "{:?}", t0.elapsed());
        assert_eq!(v["has_error"], json!(true));
        assert!(v["error_count"].as_u64().unwrap() > MAX_DIAGNOSTIC_LINES as u64, "the count is the real total: {}", v["error_count"]);
        assert!(v["first_error"].as_str().unwrap().contains("(line "));
    }

    #[test]
    fn line_numbers_come_from_the_index_while_a_parse_runs_and_from_the_count_otherwise() {
        let src = "const a = 1;\nconst b = 2;\n\nconst c = 3;\n";
        let slow: Vec<i64> = (0..=src.len() as u32).map(|o| line_of(src, o)).collect();
        let fast: Vec<i64> = { let _g = LineIndex::of(src); (0..=src.len() as u32).map(|o| line_of(src, o)).collect() };
        assert_eq!(slow, fast, "the index answers exactly what the count does, at every offset");
        assert_eq!(line_of(src, 13), 2);
        let other = String::from("x\ny\nz\n");
        { let _g = LineIndex::of(src); assert_eq!(line_of(&other, 4), 3, "a different string is counted, not read from src's index"); }
        // 20 000 top-level consts: the case that took 38 s
        let big: String = (0..20_000).map(|i| format!("const c{i} = {i};\n")).collect();
        let t = std::time::Instant::now();
        let v = parseJs(&json!(big), &json!("big.js"));
        assert!(v["nodes"].as_array().map(|n| n.len()).unwrap_or(0) > 1000, "parsed");
        assert!(t.elapsed().as_secs() < 20, "no longer quadratic: {:?}", t.elapsed());
    }
}
