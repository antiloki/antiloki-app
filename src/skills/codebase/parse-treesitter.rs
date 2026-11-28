//! parse-treesitter.rs — twin of skills/codebase/parse-treesitter.mjs, the AST-WALKING layer
//! built on top of the tree-sitter primitives ts-query.rs already converted (native `tree-sitter`
//! crate, no WASM — see that module's doc for the architecture decision this twin reuses
//! verbatim: `crate::constitution__helpers_ts_query::getTree`).
//!
//! NAMES ARE CONTRACT: `parseToTree`, `parseTreeSitter` — kept exactly as the JS source names it
//! (not snake_cased), matching the ts-query.rs precedent (`grammarForPath`/`getTree`/`queryMatches`).
//!
//! ## Scope: every grammar in ADAPTERS walks
//! The JS source's `PROFILES` table had PRECISE walk profiles for `typescript`/`tsx` (`tsWalk`),
//! `python` (`pyWalk`), `go` (`goWalk`), `elixir` (`exWalk`), `bash` (`bashWalk`), `lua`
//! (`luaWalk`), falling through to a language-agnostic `genericWalk` for every other bundled
//! grammar. This twin ported `tsWalk` and `pyWalk` (`typescript`+`tsx` share one profile there, so
//! one Rust `ts_walk` covers both), then added `rust_walk` — NOT a port, there was no `rustWalk`;
//! it exists because the substrate's own engine is Rust, and an atlas that cannot read the
//! codebase it runs on indexes the files, finds no defs, no imports and no complexity, and
//! renders a clean empty map. That is the same "healthy because it is looking at nothing" failure
//! the consistency-scan trim removed elsewhere.
//!
//! On 2026-09-11 the remaining thirteen grammars (go, java, c_sharp, c, cpp, php, ruby, kotlin,
//! swift, scala, lua, elixir, bash) got profiles in `_helpers/lang-profiles.rs` — one
//! table-driven walker, a spec per language, the same output contract. `parseTreeSitter` now
//! returns `Value::Null` only for a grammar name it has never heard of.
//!
//! This file used to carry a stronger rule: a fixture for ANY out-of-scope grammar would diverge
//! from the real JS lane "and must never be added". That rule existed to protect parity with
//! `antiloki-core`, which was deleted 2026-08-10. With one engine left there is nothing to diverge
//! FROM; the gap that remained was coverage, and it is closed.
//!
//! ## `parseToTree` collapses from `async` (JS) to sync, and returns `Tree` not `Node`
//! Same "no I/O step to await" argument as ts-query.rs's `getTree` (this fn delegates to it
//! directly). JS returns `parser.parse(source).rootNode` — a `Node`. Native `tree-sitter`'s
//! `Node<'tree>` borrows from its parent `Tree`, so a Rust fn can't return a bare `Node` without
//! also keeping its `Tree` alive; this twin returns the owning `Tree`, and callers use
//! `.root_node()` — an unavoidable, documented consequence of native tree-sitter's zero-copy
//! design (see ts-query.rs's `queryMatches`-takes-`source` note for the same category of
//! necessary Rust-only signature difference). `parseToTree` is TRANSITIVE (a `Tree` is not a
//! `serde_json::Value`) — real `pub fn` + native `#[cfg(test)]` coverage, no JSON fixture, same
//! convention as ts-query.rs's `getTree`/`queryMatches`.
//!
//! ## `parseTreeSitter` is a fixturable no-db helper
//! Unlike `parseToTree`, `parseTreeSitter`'s return shape (`{nodes, imports, calls, has_error}`)
//! is plain JSON — every field is a string/number/bool/array/null. So it gets a real
//! `conform-db` fixture (`sig: "no-db"`, 3 positional args: `source`, `path`, `grammar`) exactly
//! like any other no-db helper (e.g. `ast-parse.mjs`'s siblings). Verified against a live Node
//! run of the real JS `parseTreeSitter` this pass (dynamic import, 3 snippets: a TS file mixing
//! function/class/const-arrow-fn defs + an import + nested and top-level calls, a `typescript`-
//! grammar straight-line function, and a Python file mixing def/class/import/import-from/aliased-
//! import + calls) — every node id, span, signature, complexity, import specifier, and call
//! caller/callee matched byte-for-byte, including the subtlety that neither JS nor this twin
//! walks INTO a class body (so a Python `class Widget: def render(self): ...`'s `render` method
//! and its `draw()` call are never extracted — only top-level defs are).
use serde_json::{json, Map, Value};
use std::sync::OnceLock;
use regex::Regex;
use tree_sitter::{Node, Tree};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

/// parseToTree(source, grammar) — see module doc: sync (no I/O to await, same as ts-query.rs's
/// getTree, which this delegates to), returns the owning `Tree` (not a bare `Node`). `None`
/// covers "unsupported/scoped-out grammar" and "parse failed" — the JS source's try/catch
/// collapses both into `null`.
pub fn parseToTree(source: &str, grammar: &str) -> Option<Tree> {
    crate::constitution__helpers_ts_query::getTree(grammar, source).map(|(tree, _lang)| tree)
}

pub(crate) fn text(n: Node, src: &[u8]) -> String {
    n.utf8_text(src).unwrap_or("").to_string()
}

pub(crate) fn named_children<'tree>(n: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = n.walk();
    n.named_children(&mut cursor).collect()
}

pub(crate) fn strip_quotes(s: &str) -> String {
    let is_q = |c: char| c == '\'' || c == '"' || c == '`';
    let mut chars: Vec<char> = s.chars().collect();
    if let Some(&f) = chars.first() {
        if is_q(f) {
            chars.remove(0);
        }
    }
    if let Some(&l) = chars.last() {
        if is_q(l) {
            chars.pop();
        }
    }
    chars.into_iter().collect()
}

// ── the `nodes`/`imports`/`calls` accumulator + addDef/addImport/addCall (JS closures) ──

pub(crate) struct Out {
    pub(crate) nodes: Vec<Value>,
    pub(crate) imports: Vec<Value>,
    pub(crate) calls: Vec<Value>,
    /// definitions passed by NAME (`useReducer(storeReducer)`, `memo(Inner)`) — resolved by
    /// binding-resolve into `uses` edges when the name is a definition; kept apart from `calls`
    /// because most are locals that resolve to nothing
    pub(crate) refs: Vec<Value>,
}

pub(crate) fn span(path: &str, n: Node) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("file".into(), json!(path));
    m.insert("start_line".into(), json!(n.start_position().row as i64 + 1));
    m.insert("end_line".into(), json!(n.end_position().row as i64 + 1));
    m
}

fn add_def(out: &mut Out, path: &str, src: &[u8], kind: &str, decl: Node, name: Option<Node>, exported: bool) {
    add_def_scored(out, path, src, kind, decl, name, exported, None)
}

/// `add_def` with the cyclomatic count supplied by the caller. Languages whose branch nodes the
/// shared `ts_complexity` walk cannot read correctly (Rust — see `rust_complexity`) pass their own;
/// everyone else passes `None` and gets the shared walk.
#[allow(clippy::too_many_arguments)]
fn add_def_scored(out: &mut Out, path: &str, src: &[u8], kind: &str, decl: Node, name: Option<Node>, exported: bool, complexity: Option<i64>) {
    let Some(name_node) = name else { return };
    let name_text = text(name_node, src);
    let params = decl.child_by_field_name("parameters");
    let signature = match params {
        Some(p) => text(p, src),
        None => "()".to_string(),
    };
    let cx = if is_code_path(kind) { Some(complexity.unwrap_or_else(|| ts_complexity(decl, src))) } else { None };
    push_node(out, path, kind, &name_text, decl, signature, exported, cx);
}

/// A def kind that is a body of code (gets a cyclomatic count). Components and hooks are
/// functions in a costume — they branch like any other.
fn is_code_path(kind: &str) -> bool {
    matches!(kind, "function" | "method" | "component" | "hook")
}

/// The one place a def node is shaped. `span_node` anchors the span; `signature` is already text.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_node(out: &mut Out, path: &str, kind: &str, name: &str, span_node: Node, signature: String, exported: bool, complexity: Option<i64>) {
    let mut sp = span(path, span_node);
    if let Some(c) = complexity {
        sp.insert("complexity".into(), json!(c));
    }
    out.nodes.push(json!({
        "id": format!("{path}#{name}"),
        "path": path,
        "type": kind,
        "name": name,
        "signature": signature,
        "parent_id": path,
        "span": Value::Object(sp),
        "exported": exported,
    }));
}

pub(crate) fn add_import(out: &mut Out, path: &str, specifier: String, node: Node) {
    add_import_kind(out, path, specifier, "value", node)
}

/// `kind` says what the edge means at runtime: `value` (a plain import), `type` (erased at
/// compile time — not coupling), `dynamic` (`import('./x')` — a lazy seam, still a real edge),
/// `reexport` (`export … from './x'` — a barrel, still a real edge).
pub(crate) fn add_import_kind(out: &mut Out, path: &str, specifier: String, kind: &str, node: Node) {
    let sp = span(path, node);
    out.imports.push(json!({
        "specifier": specifier,
        "names": Value::Array(vec![]),
        "kind": kind,
        "span": Value::Object(sp),
    }));
}

pub(crate) fn add_call(out: &mut Out, path: &str, caller: Option<&str>, callee: String, node: Node) {
    let sp = span(path, node);
    out.calls.push(json!({
        "caller": caller,
        "callee": callee,
        "span": Value::Object(sp),
    }));
}

/// A JSX usage `<Child/>` is how a React component "calls" another. `via: "jsx"` lets
/// binding-resolve type the edge `renders` instead of `calls`.
fn add_render(out: &mut Out, path: &str, caller: Option<&str>, callee: String, node: Node) {
    let sp = span(path, node);
    out.calls.push(json!({
        "caller": caller,
        "callee": callee,
        "via": "jsx",
        "span": Value::Object(sp),
    }));
}

// ── cyclomatic complexity (span.complexity on function/method defs) ──
fn decision_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)(^|_)(if|elif|elsif|for|foreach|while|loop|case|when|catch|except|rescue|ternary|conditional|guard)(_|$)").unwrap()
    })
}
pub(crate) fn is_bool_op(op: &str) -> bool {
    matches!(op, "&&" | "||" | "and" | "or")
}

fn ts_complexity(decl: Node, src: &[u8]) -> i64 {
    let mut n: i64 = 1;
    let mut stack: Vec<Node> = vec![decl];
    while let Some(node) = stack.pop() {
        if node != decl && decision_re().is_match(node.kind()) {
            n += 1;
        }
        if node.kind() == "binary_expression" || node.kind() == "boolean_operator" {
            if let Some(op) = node.child_by_field_name("operator") {
                if is_bool_op(op.utf8_text(src).unwrap_or("")) {
                    n += 1;
                }
            }
        }
        for c in named_children(node) {
            stack.push(c);
        }
    }
    n
}

// ── ts/tsx precise profile (tsWalk) ──

fn ts_callee_name(call: Node, src: &[u8]) -> Option<String> {
    let f = call.child_by_field_name("function")?;
    match f.kind() {
        "identifier" => Some(text(f, src)),
        "member_expression" => f.child_by_field_name("property").map(|p| text(p, src)),
        _ => None,
    }
}

/// A JSX tag's component name: `<Row/>` → Row, `<ui.Button/>` → Button, `<div/>` → None
/// (lowercase = a host element, not a component).
fn ts_jsx_component_name(tag: Node, src: &[u8]) -> Option<String> {
    let n = tag.child_by_field_name("name")?;
    let name = match n.kind() {
        "identifier" => text(n, src),
        "member_expression" => text(n.child_by_field_name("property")?, src),
        _ => return None,
    };
    if is_pascal_case(&name) { Some(name) } else { None }
}

/// What the collector found: a call, a JSX render, or a reference (a definition passed by
/// name — `useReducer(storeReducer, …)`, `memo(Inner)`, `<Route component={Page}/>`).
enum Found<'a> {
    Call(String, Node<'a>),
    Render(String, Node<'a>),
    Ref(String, Node<'a>),
}

fn ts_collect_calls<'a>(node: Node<'a>, src: &[u8], out: &mut Vec<Found<'a>>) {
    match node.kind() {
        // `new Transform()` — constructing a class is calling it; without this a function that
        // instantiates the class beside it reads as an island
        "new_expression" => {
            if let Some(c) = node.child_by_field_name("constructor") {
                let name = match c.kind() {
                    "identifier" => Some(text(c, src)),
                    "member_expression" => c.child_by_field_name("property").map(|p| text(p, src)),
                    _ => None,
                };
                if let Some(n) = name {
                    out.push(Found::Call(n, node));
                }
            }
        }
        "call_expression" => {
            if let Some(name) = ts_callee_name(node, src) {
                out.push(Found::Call(name, node));
            }
            // bare identifiers handed to the call — most are locals and resolve to nothing at
            // binding time; the ones that name a definition become `uses` edges
            if let Some(args) = node.child_by_field_name("arguments") {
                for a in named_children(args) {
                    if a.kind() == "identifier" {
                        out.push(Found::Ref(text(a, src), a));
                    }
                }
            }
        }
        "jsx_opening_element" | "jsx_self_closing_element" => {
            if let Some(name) = ts_jsx_component_name(node, src) {
                out.push(Found::Render(name, node));
            }
        }
        _ => {}
    }
    for c in named_children(node) {
        ts_collect_calls(c, src, out);
    }
}

fn ts_record_calls(body: Option<Node>, caller: Option<&str>, path: &str, src: &[u8], out: &mut Out) {
    let Some(body) = body else { return };
    let mut found: Vec<Found> = Vec::new();
    ts_collect_calls(body, src, &mut found);
    // one `renders` edge per (caller, child) — a list renders the same row N times, that is one edge
    let mut rendered: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut referenced: std::collections::HashSet<String> = std::collections::HashSet::new();
    for f in found {
        match f {
            Found::Call(name, node) => add_call(out, path, caller, name, node),
            Found::Render(name, node) => {
                if rendered.insert(name.clone()) {
                    add_render(out, path, caller, name, node);
                }
            }
            Found::Ref(name, node) => {
                if referenced.insert(name.clone()) {
                    let sp = span(path, node);
                    out.refs.push(json!({ "caller": caller, "callee": name, "span": Value::Object(sp) }));
                }
            }
        }
    }
}

/// `import('./x')` anywhere in the file — lazy seams (`dynamic(() => import('./Impl'))`, route
/// loaders) are real edges that a top-level-only import walk never sees.
fn ts_collect_dynamic_imports(node: Node, path: &str, src: &[u8], out: &mut Out) {
    if node.kind() == "call_expression" {
        if let (Some(f), Some(args)) = (node.child_by_field_name("function"), node.child_by_field_name("arguments")) {
            if f.kind() == "import" {
                if let Some(arg) = named_children(args).into_iter().next() {
                    // only a literal target resolves; `import(name)` is a runtime decision
                    if arg.kind() == "string" {
                        add_import_kind(out, path, strip_quotes(&text(arg, src)), "dynamic", node);
                    }
                }
            }
        }
    }
    for c in named_children(node) {
        ts_collect_dynamic_imports(c, path, src, out);
    }
}

pub(crate) fn is_pascal_case(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase()) && name.chars().any(|c| c.is_ascii_lowercase())
}

fn is_hook_name(name: &str) -> bool {
    name.len() > 3 && name.starts_with("use") && name[3..].chars().next().map_or(false, |c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn ts_has_jsx(node: Node) -> bool {
    if node.kind().starts_with("jsx_") {
        return true;
    }
    named_children(node).into_iter().any(ts_has_jsx)
}

/// The React reading of a function: `useX` is a hook; a PascalCase function that renders JSX is
/// a component; everything else is a function. Applied identically by the oxc lane.
fn ts_def_kind(name: &str, body: Option<Node>) -> &'static str {
    if is_hook_name(name) {
        "hook"
    } else if is_pascal_case(name) && body.map_or(false, ts_has_jsx) {
        "component"
    } else {
        "function"
    }
}

/// Wrappers whose argument IS the component: `memo(fn)`, `forwardRef(fn)`, `observer(fn)`.
const TS_WRAPPERS: &[&str] = &["memo", "forwardRef", "React.memo", "React.forwardRef", "observer"];

/// `memo(<inner>)` → inner, for the wrapper set above.
fn ts_wrapped_arg<'t>(call: Node<'t>, src: &[u8]) -> Option<Node<'t>> {
    let f = call.child_by_field_name("function")?;
    if !TS_WRAPPERS.contains(&text(f, src).as_str()) {
        return None;
    }
    named_children(call.child_by_field_name("arguments")?).into_iter().next()
}

/// `class X extends React.Component | Component | PureComponent`.
fn ts_class_kind(node: Node, src: &[u8]) -> &'static str {
    let heritage = named_children(node).into_iter().find(|c| c.kind() == "class_heritage").map(|h| text(h, src)).unwrap_or_default();
    if heritage.contains("Component") { "component" } else { "class" }
}

fn ts_is_literal(node: Node) -> bool {
    match node.kind() {
        "string" | "number" | "true" | "false" | "null" | "undefined" | "regex" => true,
        "template_string" => !named_children(node).iter().any(|c| c.kind() == "template_substitution"),
        "unary_expression" => named_children(node).into_iter().next().map_or(false, |c| c.kind() == "number"),
        _ => false,
    }
}

/// One `const X = <value>` declarator. Returns true when a def was emitted.
fn ts_handle_const(d: Node, exported: bool, path: &str, src: &[u8], out: &mut Out) -> bool {
    let (Some(nm), Some(val)) = (d.child_by_field_name("name"), d.child_by_field_name("value")) else { return false };
    let name = text(nm, src);
    match val.kind() {
        "arrow_function" | "function_expression" => {
            let kind = ts_def_kind(&name, val.child_by_field_name("body"));
            add_def(out, path, src, kind, val, Some(nm), exported);
            ts_record_calls(val.child_by_field_name("body"), Some(&name), path, src, out);
            true
        }
        "call_expression" => match ts_wrapped_arg(val, src) {
            // `forwardRef((props, ref) => …)` / `memo(function Icon() {…})` — the wrapped fn is
            // the component; signature and complexity come from it.
            Some(inner) if inner.kind() == "arrow_function" || inner.kind() == "function_expression" => {
                let kind = if is_pascal_case(&name) { "component" } else { ts_def_kind(&name, inner.child_by_field_name("body")) };
                add_def(out, path, src, kind, inner, Some(nm), exported);
                ts_record_calls(inner.child_by_field_name("body"), Some(&name), path, src, out);
                true
            }
            // `memo(Inner)` — a component alias; nothing to score, the body lives on Inner.
            Some(inner) if inner.kind() == "identifier" && is_pascal_case(&name) => {
                push_node(out, path, "component", &name, d, format!("= {}", text(val, src)), exported, None);
                true
            }
            _ => false,
        },
        // `export const useStore = StoreContext.useContext` — a hook alias
        "member_expression" if is_hook_name(&name) => {
            let prop = val.child_by_field_name("property").map(|p| text(p, src)).unwrap_or_default();
            if prop.starts_with("use") {
                push_node(out, path, "hook", &name, d, format!("= {}", text(val, src)), exported, None);
                true
            } else {
                false
            }
        }
        _ if ts_is_literal(val) => {
            let t: String = text(val, src).chars().take(120).collect();
            push_node(out, path, "const", &name, d, format!("= {t}"), exported, None);
            true
        }
        _ => false,
    }
}

fn ts_handle_decl(node: Node, exported: bool, path: &str, src: &[u8], out: &mut Out) {
    match node.kind() {
        "function_declaration" => {
            let nm = node.child_by_field_name("name");
            let name = nm.map(|n| text(n, src)).unwrap_or_default();
            let kind = ts_def_kind(&name, node.child_by_field_name("body"));
            add_def(out, path, src, kind, node, nm, exported);
            ts_record_calls(node.child_by_field_name("body"), Some(&name), path, src, out);
        }
        "class_declaration" | "abstract_class_declaration" => {
            add_def(out, path, src, ts_class_kind(node, src), node, node.child_by_field_name("name"), exported);
        }
        "type_alias_declaration" | "interface_declaration" | "enum_declaration" => {
            add_def(out, path, src, "type", node, node.child_by_field_name("name"), exported);
        }
        "lexical_declaration" | "variable_declaration" => {
            for d in named_children(node) {
                if d.kind() != "variable_declarator" {
                    continue;
                }
                if !ts_handle_const(d, exported, path, src, out) {
                    // no def — but the initializer's calls are still real (styled.div`…`, createX(…))
                    if let Some(val) = d.child_by_field_name("value") {
                        ts_record_calls(Some(val), None, path, src, out);
                    }
                }
            }
        }
        _ => {}
    }
}

/// `import type {X} from` (statement-level) or `import {type A, type B} from` (every specifier).
fn ts_import_is_type_only(node: Node, src: &[u8]) -> bool {
    let mut c = node.walk();
    let statement_level = node.children(&mut c).take_while(|ch| ch.kind() != "import_clause").any(|ch| !ch.is_named() && text(ch, src) == "type");
    if statement_level {
        return true;
    }
    let mut specs = Vec::new();
    fn find<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>) {
        if n.kind() == "import_specifier" { out.push(n); }
        for ch in named_children(n) { find(ch, out); }
    }
    find(node, &mut specs);
    !specs.is_empty() && specs.iter().all(|sp| {
        let mut w = sp.walk();
        let r = sp.children(&mut w).any(|ch| !ch.is_named() && text(ch, src) == "type");
        r
    })
}

fn ts_walk(root: Node, path: &str, src: &[u8], out: &mut Out) {
    for node in named_children(root) {
        match node.kind() {
            "import_statement" => {
                if let Some(srcn) = node.child_by_field_name("source") {
                    let kind = if ts_import_is_type_only(node, src) { "type" } else { "value" };
                    add_import_kind(out, path, strip_quotes(&text(srcn, src)), kind, node);
                }
            }
            "export_statement" => {
                if let Some(decl) = node.child_by_field_name("declaration") {
                    ts_handle_decl(decl, true, path, src, out);
                } else if let Some(srcn) = node.child_by_field_name("source") {
                    // `export {a} from './x'` / `export * from './x'` — a barrel edge
                    let mut c = node.walk();
                    let type_only = node.children(&mut c).any(|ch| !ch.is_named() && text(ch, src) == "type");
                    let kind = if type_only { "type" } else { "reexport" };
                    add_import_kind(out, path, strip_quotes(&text(srcn, src)), kind, node);
                } else if let Some(val) = node.child_by_field_name("value") {
                    ts_record_calls(Some(val), None, path, src, out);
                }
            }
            "function_declaration" | "class_declaration" | "abstract_class_declaration" | "lexical_declaration" | "variable_declaration"
            | "type_alias_declaration" | "interface_declaration" | "enum_declaration" => {
                ts_handle_decl(node, false, path, src, out);
            }
            _ => {
                ts_record_calls(Some(node), None, path, src, out);
            }
        }
    }
    ts_collect_dynamic_imports(root, path, src, out);
}

// ── python precise profile (pyWalk) ──

fn py_callee_name(call: Node, src: &[u8]) -> Option<String> {
    let f = call.child_by_field_name("function")?;
    match f.kind() {
        "identifier" => Some(text(f, src)),
        "attribute" => f.child_by_field_name("attribute").map(|p| text(p, src)),
        _ => None,
    }
}

fn py_collect_calls<'a>(node: Node<'a>, src: &[u8], out: &mut Vec<(String, Node<'a>)>) {
    if node.kind() == "call" {
        if let Some(name) = py_callee_name(node, src) {
            out.push((name, node));
        }
    }
    for c in named_children(node) {
        py_collect_calls(c, src, out);
    }
}

fn py_record_calls(body: Option<Node>, caller: Option<&str>, path: &str, src: &[u8], out: &mut Out) {
    let Some(body) = body else { return };
    let mut found: Vec<(String, Node)> = Vec::new();
    py_collect_calls(body, src, &mut found);
    for (name, node) in found {
        add_call(out, path, caller, name, node);
    }
}

fn py_walk(root: Node, path: &str, src: &[u8], out: &mut Out) {
    for node in named_children(root) {
        match node.kind() {
            "function_definition" => {
                let nm = node.child_by_field_name("name");
                add_def(out, path, src, "function", node, nm, true);
                let nm_text = nm.map(|n| text(n, src));
                py_record_calls(node.child_by_field_name("body"), nm_text.as_deref(), path, src, out);
            }
            "class_definition" => {
                add_def(out, path, src, "class", node, node.child_by_field_name("name"), true);
            }
            "import_statement" => {
                for c in named_children(node) {
                    if c.kind() == "dotted_name" {
                        add_import(out, path, text(c, src), node);
                    } else if c.kind() == "aliased_import" {
                        if let Some(n2) = c.child_by_field_name("name") {
                            add_import(out, path, text(n2, src), node);
                        }
                    }
                }
            }
            "import_from_statement" => {
                if let Some(modn) = node.child_by_field_name("module_name") {
                    add_import(out, path, text(modn, src), node);
                }
            }
            _ => {
                py_record_calls(Some(node), None, path, src, out);
            }
        }
    }
}

// ── rust precise profile (rustWalk) ──
//
// NO JS TWIN. The JS lane never parsed Rust — it deferred every grammar past tsx/typescript/python
// — and that lane is deleted, so this profile is not a port and has nothing to stay faithful to.
// The contract it answers is the atlas's own: {nodes, imports, calls}.

fn rust_is_pub(node: Node) -> bool {
    named_children(node).iter().any(|c| c.kind() == "visibility_modifier")
}

fn rust_callee_from(f: Node, src: &[u8]) -> Option<String> {
    match f.kind() {
        "identifier" => Some(text(f, src)),
        // `x.foo()` — the call graph wants the method, not the receiver.
        "field_expression" => f.child_by_field_name("field").map(|p| text(p, src)),
        // `Foo::bar()`, `crate::a::b()` — the last segment is the callee.
        "scoped_identifier" => f.child_by_field_name("name").map(|p| text(p, src)),
        // `foo::<T>()` — unwrap the turbofish and ask again.
        "generic_function" => f.child_by_field_name("function").and_then(|g| rust_callee_from(g, src)),
        _ => None,
    }
}

fn rust_collect_calls<'a>(node: Node<'a>, src: &[u8], out: &mut Vec<(String, Node<'a>)>) {
    match node.kind() {
        "call_expression" => {
            if let Some(f) = node.child_by_field_name("function") {
                if let Some(name) = rust_callee_from(f, src) {
                    out.push((name, node));
                }
            }
        }
        // A macro IS the call site in a great deal of Rust (`json!`, `format!`, `write!`). Kept,
        // with the `!` retained so a reader can tell the two edge kinds apart in the graph.
        "macro_invocation" => {
            if let Some(m) = node.child_by_field_name("macro") {
                out.push((format!("{}!", text(m, src)), node));
            }
        }
        _ => {}
    }
    for c in named_children(node) {
        rust_collect_calls(c, src, out);
    }
}

fn rust_record_calls(body: Option<Node>, caller: Option<&str>, path: &str, src: &[u8], out: &mut Out) {
    let Some(body) = body else { return };
    let mut found: Vec<(String, Node)> = Vec::new();
    rust_collect_calls(body, src, &mut found);
    for (name, node) in found {
        add_call(out, path, caller, name, node);
    }
}

// Rust's own cyclomatic count, NOT the shared `decision_re` walk. That walk matches on node-kind
// NAME, and Rust spells one branch as `match_expression` wrapping N `match_arm`s — a name-matching
// regex counts both, adding a phantom decision to every match. Arms are the branches (the same
// reading that makes `switch_case` count in TS while `switch_statement` does not).
//
// `?` is deliberately NOT counted: it propagates rather than branches, and counting it would
// inflate every Rust fn against the JS/Python baseline that gravity and the coverage ranking
// compare against — a metric is only useful here if it means the same thing across languages.
fn rust_complexity(decl: Node, src: &[u8]) -> i64 {
    let mut n: i64 = 1;
    let mut stack: Vec<Node> = vec![decl];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "if_expression" | "while_expression" | "for_expression" | "loop_expression" | "match_arm" => n += 1,
            "binary_expression" => {
                if let Some(op) = node.child_by_field_name("operator") {
                    if is_bool_op(op.utf8_text(src).unwrap_or("")) {
                        n += 1;
                    }
                }
            }
            _ => {}
        }
        for c in named_children(node) {
            stack.push(c);
        }
    }
    n
}

fn rust_walk(root: Node, path: &str, src: &[u8], out: &mut Out) {
    rust_walk_items(root, path, src, out, false);
}

fn rust_walk_items(parent: Node, path: &str, src: &[u8], out: &mut Out, in_impl: bool) {
    for node in named_children(parent) {
        match node.kind() {
            "function_item" => {
                let nm = node.child_by_field_name("name");
                let kind = if in_impl { "method" } else { "function" };
                add_def_scored(out, path, src, kind, node, nm, rust_is_pub(node), Some(rust_complexity(node, src)));
                let nm_text = nm.map(|n| text(n, src));
                rust_record_calls(node.child_by_field_name("body"), nm_text.as_deref(), path, src, out);
            }
            // The type-shaped bucket. `class` is the atlas's existing vocabulary (code-decorate's
            // NODE_KIND), so these ride the lane the FE already renders rather than inventing one.
            "struct_item" | "enum_item" | "union_item" | "trait_item" => {
                add_def(out, path, src, "class", node, node.child_by_field_name("name"), rust_is_pub(node));
            }
            "const_item" | "static_item" => {
                add_def(out, path, src, "const", node, node.child_by_field_name("name"), rust_is_pub(node));
            }
            "type_item" => {
                add_def(out, path, src, "type", node, node.child_by_field_name("name"), rust_is_pub(node));
            }
            // `impl Foo { … }` is not itself a definition — the fns inside it are, and they are
            // methods rather than free functions.
            "impl_item" => {
                if let Some(body) = node.child_by_field_name("body") {
                    rust_walk_items(body, path, src, out, true);
                }
            }
            "mod_item" => match node.child_by_field_name("body") {
                // `mod foo { … }` — inline, so its items belong to THIS file.
                Some(body) => rust_walk_items(body, path, src, out, in_impl),
                // `mod foo;` — a declaration pointing at another file. That is an EDGE, not a
                // definition; dep-resolve turns the bare name into foo.rs / foo/mod.rs.
                None => {
                    if let Some(nm) = node.child_by_field_name("name") {
                        add_import(out, path, text(nm, src), node);
                    }
                }
            },
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    add_import(out, path, text(arg, src), node);
                }
            }
            "macro_definition" => {
                add_def(out, path, src, "function", node, node.child_by_field_name("name"), rust_is_pub(node));
            }
            _ => {
                rust_record_calls(Some(node), None, path, src, out);
            }
        }
    }
}

/// The file node carries the file's own span so `loc` is a measurement rather than 0 — the
/// atlas reads `span_json.end_line` as the file's length. Line count, not the parser's last row:
/// a trailing newline would otherwise make the two lanes disagree by one.
pub(crate) fn file_node(path: &str, source: &str) -> Value {
    let lines = source.lines().count().max(1);
    json!({
        "id": path, "path": path, "type": "file", "name": path,
        "span": { "file": path, "start_line": 1, "end_line": lines },
    })
}

/// parseTreeSitter(source, path, grammar) — no-db helper, 3 positional args. Null only for a
/// grammar with no walk profile at all (see module doc "Scope").
#[allow(non_snake_case)]
pub fn parseTreeSitter(source: &Value, path: &Value, grammar: &Value) -> Value {
    let source_s = str_qq(Some(source));
    let path_s = str_qq(Some(path));
    let grammar_s = str_qq(Some(grammar));

    let profile = crate::codebase__helpers_lang_profiles::profile_for(&grammar_s);
    match grammar_s.as_str() {
        "tsx" | "typescript" | "python" | "rust" => {}
        _ if profile.is_some() => {}
        _ => return Value::Null, // no walk profile — see module doc "Scope" section
    }

    let Some(tree) = parseToTree(&source_s, &grammar_s) else {
        return Value::Null;
    };
    let root = tree.root_node();
    let src_bytes = source_s.as_bytes();

    let mut out = Out {
        nodes: vec![file_node(&path_s, &source_s)],
        imports: Vec::new(),
        calls: Vec::new(),
        refs: Vec::new(),
    };

    match (grammar_s.as_str(), profile) {
        ("python", _) => py_walk(root, &path_s, src_bytes, &mut out),
        ("rust", _) => rust_walk(root, &path_s, src_bytes, &mut out),
        (_, Some(p)) => crate::codebase__helpers_lang_profiles::walk(p, root, &path_s, src_bytes, &mut out),
        _ => ts_walk(root, &path_s, src_bytes, &mut out),
    }

    json!({
        "nodes": out.nodes,
        "imports": out.imports,
        "calls": out.calls,
        "refs": out.refs,
        "has_error": root.has_error(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        json!(v)
    }

    #[test]
    fn rust_items_uses_mod_decls_impl_methods_and_calls() {
        let source = concat!(
            "use serde_json::Value;\n",
            "mod helper;\n",
            "\n",
            "pub struct Widget { n: i64 }\n",
            "enum Mode { A, B }\n",
            "pub const CAP: i64 = 10;\n",
            "type Alias = Vec<u8>;\n",
            "\n",
            "pub fn add(a: i64, b: i64) -> i64 {\n",
            "    if a > 0 && b > 0 {\n",
            "        return helper::sum(a, b);\n",
            "    }\n",
            "    b\n",
            "}\n",
            "\n",
            "impl Widget {\n",
            "    fn render(&self) -> String {\n",
            "        format!(\"{}\", self.n)\n",
            "    }\n",
            "}\n",
        );
        let result = parseTreeSitter(&s(source), &s("foo.rs"), &s("rust"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes[0]["type"], json!("file"));
        assert_eq!(nodes[0]["span"]["start_line"], json!(1), "the file node carries its own span so loc is a measurement");
        let by_name = |n: &str| nodes.iter().find(|x| x["name"] == json!(n)).cloned().unwrap();

        // pub rides `visibility_modifier`, so `exported` is real rather than always-true.
        assert_eq!(by_name("Widget")["type"], json!("class"));
        assert_eq!(by_name("Widget")["exported"], json!(true));
        assert_eq!(by_name("Mode")["type"], json!("class"));
        assert_eq!(by_name("Mode")["exported"], json!(false));
        assert_eq!(by_name("CAP")["type"], json!("const"));
        assert_eq!(by_name("Alias")["type"], json!("type"));

        // A free fn is a function; the same node inside `impl` is a method.
        assert_eq!(by_name("add")["type"], json!("function"));
        assert_eq!(by_name("render")["type"], json!("method"));
        // base + if + `&&` — and NOT a phantom from the shared name-matching walk.
        assert_eq!(by_name("add")["span"]["complexity"], json!(3));
        // a type is not a code path
        assert!(by_name("Widget")["span"].get("complexity").is_none());

        // `use` carries the path; `mod foo;` is an EDGE to another file, not a definition.
        let imports = result["imports"].as_array().unwrap();
        let specs: Vec<&str> = imports.iter().map(|i| i["specifier"].as_str().unwrap()).collect();
        assert!(specs.contains(&"serde_json::Value"), "use path: {specs:?}");
        assert!(specs.contains(&"helper"), "mod decl becomes an import edge: {specs:?}");
        assert!(!nodes.iter().any(|n| n["name"] == json!("helper")), "`mod foo;` is an edge, never a def");

        // `helper::sum(..)` resolves to the last segment; the macro keeps its `!`.
        let calls = result["calls"].as_array().unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c["callee"].as_str().unwrap()).collect();
        assert!(callees.contains(&"sum"), "scoped call: {callees:?}");
        assert!(callees.contains(&"format!"), "macro invocation: {callees:?}");
        let sum = calls.iter().find(|c| c["callee"] == json!("sum")).unwrap();
        assert_eq!(sum["caller"], json!("add"));
        assert_eq!(result["has_error"], json!(false));
    }

    #[test]
    fn rust_match_arms_count_once_not_twice() {
        // The regression this profile's own complexity fn exists to prevent: the shared
        // name-matching walk would count `match_expression` AND each `match_arm`.
        let source = "fn pick(x: i64) -> i64 {\n    match x {\n        0 => 1,\n        1 => 2,\n        _ => 3,\n    }\n}\n";
        let result = parseTreeSitter(&s(source), &s("m.rs"), &s("rust"));
        let f = result["nodes"].as_array().unwrap().iter().find(|n| n["name"] == json!("pick")).cloned().unwrap();
        assert_eq!(f["span"]["complexity"], json!(4)); // base + 3 arms, no phantom for the match itself
    }

    #[test]
    fn rust_inline_mod_items_belong_to_the_same_file() {
        let source = "mod inner {\n    pub fn deep() -> i64 { 1 }\n}\n";
        let result = parseTreeSitter(&s(source), &s("n.rs"), &s("rust"));
        let nodes = result["nodes"].as_array().unwrap();
        assert!(nodes.iter().any(|n| n["name"] == json!("deep") && n["path"] == json!("n.rs")));
        assert!(result["imports"].as_array().unwrap().is_empty(), "an inline mod is not an edge");
    }

    #[test]
    fn ts_function_class_arrow_import_and_calls() {
        let source = "import { helper } from './helper';\n\nexport function add(a, b) {\n  if (a > 0) {\n    return helper(a, b);\n  }\n  return b;\n}\n\nexport class Widget {\n  render() {\n    return draw();\n  }\n}\n\nexport const mul = (a, b) => {\n  for (let i = 0; i < a; i++) {\n    log(i);\n  }\n  return a * b;\n};\n\ntopLevelCall(1, 2);\n";
        let result = parseTreeSitter(&s(source), &s("foo.ts"), &s("tsx"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 4); // file + add + Widget + mul
        assert_eq!(nodes[0]["type"], json!("file"));
        assert_eq!(nodes[0]["span"]["start_line"], json!(1), "the file node carries its own span so loc is a measurement");
        assert_eq!(nodes[1]["name"], json!("add"));
        assert_eq!(nodes[1]["type"], json!("function"));
        assert_eq!(nodes[1]["signature"], json!("(a, b)"));
        assert_eq!(nodes[1]["span"]["complexity"], json!(2)); // base + if
        assert_eq!(nodes[1]["exported"], json!(true));
        assert_eq!(nodes[2]["name"], json!("Widget"));
        assert_eq!(nodes[2]["type"], json!("class"));
        assert!(nodes[2]["span"].get("complexity").is_none()); // class is not a code path
        assert_eq!(nodes[3]["name"], json!("mul"));
        assert_eq!(nodes[3]["span"]["complexity"], json!(2)); // base + for

        let imports = result["imports"].as_array().unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0]["specifier"], json!("./helper"));
        assert_eq!(imports[0]["span"]["start_line"], json!(1));

        let calls = result["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0], json!({ "caller": "add", "callee": "helper", "span": { "file": "foo.ts", "start_line": 5, "end_line": 5 } }));
        assert_eq!(calls[1], json!({ "caller": "mul", "callee": "log", "span": { "file": "foo.ts", "start_line": 18, "end_line": 18 } }));
        assert_eq!(calls[2], json!({ "caller": null, "callee": "topLevelCall", "span": { "file": "foo.ts", "start_line": 23, "end_line": 23 } }));
        // `draw()` inside Widget.render is never extracted — class bodies aren't walked (matches JS).
        assert!(!calls.iter().any(|c| c["callee"] == json!("draw")));
        assert_eq!(result["has_error"], json!(false));
    }

    #[test]
    fn ts_straight_line_function_complexity_one_typescript_grammar() {
        let source = "function straight() {\n  return 1;\n}\n";
        let result = parseTreeSitter(&s(source), &s("bar.ts"), &s("typescript"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1]["name"], json!("straight"));
        assert_eq!(nodes[1]["exported"], json!(false));
        assert_eq!(nodes[1]["span"]["complexity"], json!(1));
    }

    #[test]
    fn python_def_class_imports_aliased_import_and_calls() {
        let source = "import os\nimport numpy as np\nfrom collections import OrderedDict\n\ndef compute(x):\n    if x > 0:\n        return helper(x)\n    for i in range(x):\n        log(i)\n    return x\n\nclass Widget:\n    def render(self):\n        return draw()\n\ntop_level_call(1, 2)\n";
        let result = parseTreeSitter(&s(source), &s("foo.py"), &s("python"));
        let nodes = result["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3); // file + compute + Widget (render is nested, not extracted)
        assert_eq!(nodes[1]["name"], json!("compute"));
        assert_eq!(nodes[1]["span"]["complexity"], json!(3)); // base + if + for
        assert_eq!(nodes[1]["exported"], json!(true));
        assert_eq!(nodes[2]["name"], json!("Widget"));
        assert_eq!(nodes[2]["type"], json!("class"));

        let imports = result["imports"].as_array().unwrap();
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0]["specifier"], json!("os"));
        assert_eq!(imports[1]["specifier"], json!("numpy")); // aliased_import 'name' field, not the alias
        assert_eq!(imports[2]["specifier"], json!("collections"));

        let calls = result["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0]["callee"], json!("helper"));
        assert_eq!(calls[0]["caller"], json!("compute"));
        assert_eq!(calls[1]["callee"], json!("range"));
        assert_eq!(calls[2]["callee"], json!("log"));
        assert_eq!(calls[3], json!({ "caller": null, "callee": "top_level_call", "span": { "file": "foo.py", "start_line": 16, "end_line": 16 } }));
        // draw() inside Widget.render is never extracted (class bodies aren't walked, matches JS).
        assert!(!calls.iter().any(|c| c["callee"] == json!("draw")));
    }

    #[test]
    fn unknown_grammar_returns_null_never_panics() {
        assert_eq!(parseTreeSitter(&s("x"), &s("a.txt"), &s("nonsense")), Value::Null);
        // once out of scope, now walked — see profile_tests below
        assert!(!parseTreeSitter(&s("package main"), &s("main.go"), &s("go")).is_null());
        assert!(!parseTreeSitter(&s("puts 1"), &s("a.rb"), &s("ruby")).is_null());
    }

    #[test]
    fn parse_to_tree_returns_owning_tree_and_none_for_unsupported_grammar() {
        let tree = parseToTree("const x = 1;", "tsx").expect("tsx should parse");
        assert_eq!(tree.root_node().kind(), "program");
        assert!(parseToTree("package main", "unknown-grammar-xyz").is_none());
    }

    #[test]
    fn strip_quotes_strips_one_leading_and_one_trailing_quote_char() {
        assert_eq!(strip_quotes("'./helper'"), "./helper");
        assert_eq!(strip_quotes("\"./helper\""), "./helper");
        assert_eq!(strip_quotes("`./helper`"), "./helper");
        assert_eq!(strip_quotes("bare"), "bare");
    }
}



// ── the thirteen lang-profiles.rs languages, through the same entry point ──
//
// Each sample is a small file exercising defs (function / method / class / const / type),
// visibility, imports, calls-with-caller and the per-language branch kinds. The expected numbers
// were checked by hand against the source, not copied from a first run.
#[cfg(test)]
mod profile_tests {
    use super::*;

    struct Parsed {
        v: Value,
    }
    impl Parsed {
        fn of(src: &str, path: &str, grammar: &str) -> Self {
            let v = parseTreeSitter(&json!(src), &json!(path), &json!(grammar));
            assert!(!v.is_null(), "{grammar} must have a walk profile");
            Self { v }
        }
        fn def(&self, name: &str) -> Value {
            self.v["nodes"].as_array().unwrap().iter().find(|n| n["name"] == json!(name)).cloned().unwrap_or_else(|| panic!("no def named {name}: {:?}", self.names()))
        }
        fn names(&self) -> Vec<String> {
            self.v["nodes"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap().to_string()).collect()
        }
        fn has_def(&self, name: &str) -> bool {
            self.v["nodes"].as_array().unwrap().iter().any(|n| n["name"] == json!(name))
        }
        fn imports(&self) -> Vec<String> {
            self.v["imports"].as_array().unwrap().iter().map(|i| i["specifier"].as_str().unwrap().to_string()).collect()
        }
        fn calls(&self) -> Vec<(Option<String>, String)> {
            self.v["calls"].as_array().unwrap().iter().map(|c| (c["caller"].as_str().map(|s| s.to_string()), c["callee"].as_str().unwrap().to_string())).collect()
        }
        fn call(&self, caller: Option<&str>, callee: &str) -> bool {
            self.calls().iter().any(|(cr, ce)| cr.as_deref() == caller && ce == callee)
        }
        fn has_error(&self) -> bool {
            self.v["has_error"] == json!(true)
        }
    }

    #[test]
    fn go_funcs_methods_types_consts_imports_calls() {
        let p = Parsed::of(GO_SRC, "a.go", "go");
        assert!(!p.has_error());
        assert_eq!(p.def("Add")["type"], json!("function"));
        assert_eq!(p.def("Add")["exported"], json!(true), "capitalised = exported");
        assert_eq!(p.def("Add")["signature"], json!("(a, b int)"));
        // base + if + && + for + case + default
        assert_eq!(p.def("Add")["span"]["complexity"], json!(6));
        assert_eq!(p.def("render")["type"], json!("method"), "receiver = method");
        assert_eq!(p.def("render")["exported"], json!(false));
        assert_eq!(p.def("Widget")["type"], json!("class"));
        assert_eq!(p.def("Shape")["type"], json!("class"), "interface is type-shaped");
        assert_eq!(p.def("Mode")["type"], json!("type"), "a named non-struct is a type alias");
        assert_eq!(p.def("Cap")["type"], json!("const"));
        assert_eq!(p.def("count")["type"], json!("const"));
        assert_eq!(p.imports(), vec!["fmt", "github.com/acme/app/util", "os"]);
        assert!(p.call(Some("Add"), "Sum"), "selector call → field name");
        assert!(p.call(Some("render"), "Sprint"));
        assert!(p.call(Some("init"), "helper"));
    }

    #[test]
    fn java_class_body_methods_modifiers_static_import() {
        let p = Parsed::of(JAVA_SRC, "A.java", "java");
        assert!(!p.has_error());
        assert_eq!(p.def("Widget")["type"], json!("class"));
        assert_eq!(p.def("Widget")["exported"], json!(true));
        assert_eq!(p.def("add")["type"], json!("method"));
        assert_eq!(p.def("add")["signature"], json!("(int a, int b)"));
        // base + if + && + for + case (default label does not count)
        assert_eq!(p.def("add")["span"]["complexity"], json!(5));
        assert_eq!(p.def("helper")["exported"], json!(false), "private");
        assert_eq!(p.def("Shape")["exported"], json!(false), "package-private interface");
        assert_eq!(p.def("area")["exported"], json!(true), "interface members are implicitly public");
        assert_eq!(p.def("Mode")["type"], json!("class"));
        assert_eq!(p.def("Point")["type"], json!("class"));
        assert_eq!(p.imports(), vec!["java.util.List", "static com.acme.Util.sum"]);
        assert!(p.call(Some("add"), "sum"));
        assert!(p.call(Some("add"), "helper"), "this.helper(b) → helper");
    }

    #[test]
    fn csharp_namespaces_modifiers_usings() {
        let p = Parsed::of(CS_SRC, "A.cs", "c_sharp");
        assert!(!p.has_error());
        assert_eq!(p.def("Widget")["type"], json!("class"));
        assert_eq!(p.def("Add")["type"], json!("method"));
        // base + if + && + foreach + case section (default section does not count)
        assert_eq!(p.def("Add")["span"]["complexity"], json!(5));
        assert_eq!(p.def("Helper")["exported"], json!(false));
        assert_eq!(p.def("Mode")["exported"], json!(false), "no modifier = internal");
        assert_eq!(p.def("Area")["exported"], json!(true), "interface member");
        assert!(p.has_def("Other"), "file-scoped namespace members are still walked");
        assert_eq!(p.imports(), vec!["System", "System.Collections.Generic", "static System.Math"]);
        assert!(p.call(Some("Add"), "Sum"), "Util.Sum → Sum");
        assert!(p.call(Some("Add"), "Log"));
    }

    #[test]
    fn c_static_linkage_typedefs_includes() {
        let p = Parsed::of(C_SRC, "a.c", "c");
        assert!(!p.has_error());
        assert_eq!(p.def("add")["type"], json!("function"));
        assert_eq!(p.def("add")["exported"], json!(false), "static = file-local");
        assert_eq!(p.def("main")["exported"], json!(true));
        assert_eq!(p.def("main")["signature"], json!("(void)"));
        // base + if + && + for + case
        assert_eq!(p.def("add")["span"]["complexity"], json!(5));
        assert_eq!(p.def("widget")["type"], json!("class"));
        assert_eq!(p.def("mode")["type"], json!("class"));
        assert_eq!(p.def("widget_t")["type"], json!("type"));
        assert_eq!(p.def("CAP")["type"], json!("const"), "#define");
        assert_eq!(p.imports(), vec!["<stdio.h>", "util.h"], "system header keeps its brackets for resolve_c");
        assert!(p.call(Some("main"), "add"));
        assert!(p.call(Some("add"), "printf"));
    }

    #[test]
    fn cpp_class_access_sections_and_out_of_line_members() {
        let p = Parsed::of(CPP_SRC, "a.cpp", "cpp");
        assert!(!p.has_error());
        assert_eq!(p.def("Widget")["type"], json!("class"));
        assert_eq!(p.def("Widget")["signature"], json!("()"), "a class has no parameter list");
        assert_eq!(p.def("add")["type"], json!("method"));
        assert_eq!(p.def("add")["exported"], json!(true), "under public:");
        // base + if + && + range-for
        assert_eq!(p.def("add")["span"]["complexity"], json!(4));
        assert_eq!(p.def("helper")["type"], json!("method"), "Widget::helper defined out of line");
        assert_eq!(p.def("twice")["type"], json!("function"), "template function");
        assert_eq!(p.def("Pt")["type"], json!("class"));
        assert!(p.call(Some("add"), "sum"), "util::sum → sum");
        assert!(p.call(Some("add"), "helper"), "this->helper → helper");
        assert!(p.call(Some("main"), "twice"));
    }

    #[test]
    fn php_namespaces_use_groups_requires_and_visibility() {
        let p = Parsed::of(PHP_SRC, "a.php", "php");
        assert!(!p.has_error());
        assert_eq!(p.def("add")["type"], json!("function"));
        assert_eq!(p.def("add")["signature"], json!("(int $a, int $b)"));
        // base + if + && + foreach + case
        assert_eq!(p.def("add")["span"]["complexity"], json!(5));
        assert_eq!(p.def("__construct")["type"], json!("method"));
        assert_eq!(p.def("helper")["exported"], json!(false));
        assert_eq!(p.def("render")["exported"], json!(true));
        assert_eq!(p.def("CAP")["type"], json!("const"));
        for t in ["Widget", "Shape", "Loggable", "Mode"] {
            assert_eq!(p.def(t)["type"], json!("class"), "{t}");
        }
        assert_eq!(p.imports(), vec!["Acme\\Util\\Helper", "Acme\\Util\\Sum", "Acme\\Util\\Log", "bootstrap.php", "config.php"]);
        assert!(p.call(Some("add"), "sum"), "Helper::sum → sum");
        assert!(p.call(Some("render"), "helper"), "$this->helper() → helper");
    }

    #[test]
    fn ruby_private_section_singleton_methods_requires() {
        let p = Parsed::of(RB_SRC, "a.rb", "ruby");
        assert!(!p.has_error());
        assert_eq!(p.def("add")["type"], json!("function"));
        // base + if + && + when + unless-modifier
        assert_eq!(p.def("add")["span"]["complexity"], json!(5));
        assert_eq!(p.def("render")["type"], json!("method"));
        assert_eq!(p.def("render")["exported"], json!(true));
        assert_eq!(p.def("build")["type"], json!("method"), "def self.build");
        assert_eq!(p.def("helper")["exported"], json!(false), "after `private`");
        assert_eq!(p.def("Acme")["type"], json!("class"), "module is type-shaped");
        assert_eq!(p.def("run")["type"], json!("method"));
        assert_eq!(p.imports(), vec!["json", "util/helper", "x.rb"]);
        assert!(p.call(Some("add"), "sum"), "Helper.sum → sum");
        assert!(p.call(Some("add"), "log"), "inside a block");
        assert!(p.call(Some("run"), "add"));
    }

    #[test]
    fn kotlin_classes_objects_companions_visibility() {
        let p = Parsed::of(KT_SRC, "a.kt", "kotlin");
        assert!(!p.has_error(), "{names:?}", names = p.names());
        assert_eq!(p.def("add")["type"], json!("function"));
        assert_eq!(p.def("add")["signature"], json!("(a: Int, b: Int)"));
        // base + if + && + for + when arm (the else arm does not count)
        assert_eq!(p.def("add")["span"]["complexity"], json!(5));
        assert_eq!(p.def("Widget")["type"], json!("class"));
        assert_eq!(p.def("render")["type"], json!("method"));
        assert_eq!(p.def("helper")["exported"], json!(false));
        assert_eq!(p.def("build")["type"], json!("method"), "companion object member");
        assert_eq!(p.def("Registry")["type"], json!("class"), "object");
        assert_eq!(p.def("get")["type"], json!("method"));
        assert_eq!(p.def("Handler")["type"], json!("type"));
        assert_eq!(p.def("CAP")["type"], json!("const"));
        assert_eq!(p.imports(), vec!["java.util.List", "com.acme.util.sum"]);
        assert!(p.call(Some("render"), "helper"), "Util.helper(n) → helper");
        assert!(p.call(Some("build"), "Widget"));
    }

    #[test]
    fn kotlin_single_line_body_is_salvaged_and_flagged() {
        // kotlin-ng 1.1.0 needs a newline before `}` on a member line. The definitions inside the
        // resulting ERROR node are still extracted; the file is honestly marked has_error.
        let p = Parsed::of("interface Shape { fun area(): Double }\n", "s.kt", "kotlin");
        assert!(p.has_error());
        assert!(p.has_def("area"), "{names:?}", names = p.names());
    }

    #[test]
    fn swift_extensions_protocols_guards() {
        let p = Parsed::of(SWIFT_SRC, "a.swift", "swift");
        assert!(!p.has_error());
        assert_eq!(p.def("add")["signature"], json!("(_ a: Int, _ b: Int)"));
        // base + if + && + for + case + guard
        assert_eq!(p.def("add")["span"]["complexity"], json!(6));
        assert_eq!(p.def("init")["type"], json!("method"));
        assert_eq!(p.def("init")["signature"], json!("(n: Int)"));
        assert_eq!(p.def("helper")["exported"], json!(false));
        assert_eq!(p.def("build")["type"], json!("method"), "static func");
        assert_eq!(p.def("extra")["type"], json!("method"), "declared in an extension");
        assert_eq!(p.names().iter().filter(|n| *n == "Widget").count(), 1, "extension Widget is not a second Widget symbol");
        assert_eq!(p.def("Shape")["type"], json!("class"), "protocol");
        assert_eq!(p.def("area")["type"], json!("method"));
        assert_eq!(p.def("Handler")["type"], json!("type"));
        assert_eq!(p.def("cap")["type"], json!("const"));
        assert_eq!(p.imports(), vec!["Foundation", "UIKit"]);
        assert!(p.call(Some("add"), "sum"), "Util.sum → sum");
        assert!(p.call(Some("render"), "helper"), "self.helper() → helper");
    }

    #[test]
    fn scala_objects_traits_case_classes_selector_imports() {
        let p = Parsed::of(SCALA_SRC, "a.scala", "scala");
        assert!(!p.has_error());
        assert_eq!(p.def("add")["type"], json!("function"));
        // base + if + && + for + 2 case clauses
        assert_eq!(p.def("add")["span"]["complexity"], json!(6));
        assert_eq!(p.def("Registry")["type"], json!("class"), "object");
        assert_eq!(p.def("get")["type"], json!("method"));
        assert_eq!(p.def("helper")["exported"], json!(false));
        assert_eq!(p.def("Shape")["type"], json!("class"), "trait");
        assert_eq!(p.def("Point")["type"], json!("class"), "case class");
        assert_eq!(p.def("Handler")["type"], json!("type"));
        assert_eq!(p.imports(), vec!["java.util.List", "com.acme.util.sum", "com.acme.util.log"], "selectors expand");
        assert!(p.call(Some("render"), "helper"));
    }

    #[test]
    fn lua_local_functions_methods_requires() {
        let p = Parsed::of(LUA_SRC, "a.lua", "lua");
        assert!(!p.has_error());
        assert_eq!(p.def("add")["exported"], json!(true), "global function");
        // base + if + and + elseif + for + while
        assert_eq!(p.def("add")["span"]["complexity"], json!(6));
        assert_eq!(p.def("helper")["exported"], json!(false), "local function");
        assert_eq!(p.def("mul")["exported"], json!(false), "local x = function");
        assert_eq!(p.def("mul")["signature"], json!("(a, b)"));
        assert_eq!(p.def("Widget.new")["type"], json!("function"));
        assert_eq!(p.def("Widget:render")["type"], json!("method"), "colon form");
        assert_eq!(p.imports(), vec!["json", "acme.util", "side_effect"]);
        assert!(p.call(Some("add"), "sum"), "util.sum → sum");
        assert!(p.call(Some("Widget:render"), "helper"), "self:helper() → helper");
        assert!(p.call(None, "add"), "top-level call");
    }

    #[test]
    fn elixir_modules_defs_attributes_aliases() {
        let p = Parsed::of(EX_SRC, "a.ex", "elixir");
        assert!(!p.has_error());
        assert_eq!(p.def("Acme.Widget")["type"], json!("class"));
        assert_eq!(p.def("add")["type"], json!("function"), "Elixir has functions, not methods");
        assert_eq!(p.def("add")["signature"], json!("(a, b)"), "guard stripped");
        // base + if + and + 2 case arms + 2 cond arms
        assert_eq!(p.def("add")["span"]["complexity"], json!(7));
        assert_eq!(p.def("helper")["exported"], json!(false), "defp");
        assert_eq!(p.def("build")["type"], json!("function"), "defmacro");
        assert_eq!(p.def("render")["signature"], json!("(%{n: n})"));
        assert_eq!(p.def("cap")["type"], json!("const"), "@cap module attribute");
        assert_eq!(p.imports(), vec!["GenServer", "Enum", "Acme.Util.Helper", "Logger"]);
        assert!(p.call(Some("add"), "sum"), "Helper.sum → sum");
        assert!(p.call(Some("helper"), "log"), "keyword `do:` body");
        assert!(p.call(Some("render"), "info"), "Logger.info → info");
        assert!(p.call(Some("run"), "add"));
        assert!(!p.calls().iter().any(|(_, c)| c == "if" || c == "case" || c == "cond"), "control-flow forms are not call edges");
    }

    #[test]
    fn bash_functions_sources_commands() {
        let p = Parsed::of(SH_SRC, "a.sh", "bash");
        assert!(!p.has_error());
        assert_eq!(p.def("add")["type"], json!("function"));
        // base + if + && + elif + for + while + 2 case items + ||
        assert_eq!(p.def("add")["span"]["complexity"], json!(9));
        assert_eq!(p.def("helper")["type"], json!("function"), "the `function name` form");
        assert_eq!(p.imports(), vec!["./lib/util.sh", "$HOME/.config/env"]);
        assert!(p.call(Some("add"), "sum"));
        assert!(p.call(Some("add"), "log"));
        assert!(p.call(None, "main"));
        assert!(!p.calls().iter().any(|(_, c)| c == "echo"), "builtins are not edges");
    }

    const GO_SRC: &str = r#"package main

import (
	"fmt"
	"github.com/acme/app/util"
)
import "os"

const Cap = 10
var count int

type Widget struct{ n int }
type Mode int
type Shape interface{ Area() float64 }

func Add(a, b int) int {
	if a > 0 && b > 0 {
		return util.Sum(a, b)
	}
	for i := 0; i < a; i++ {
		fmt.Println(i)
	}
	switch a {
	case 1:
		return 1
	default:
	}
	return b
}

func (w *Widget) render() string { return fmt.Sprint(w.n) }

func init() { helper() }
"#;
    const JAVA_SRC: &str = r#"package com.acme;

import java.util.List;
import static com.acme.Util.sum;

public class Widget extends Base {
    private int n;
    public Widget(int n) { this.n = n; }
    public int add(int a, int b) {
        if (a > 0 && b > 0) { return sum(a, b); }
        for (int i = 0; i < a; i++) { log(i); }
        switch (a) { case 1: return 1; default: break; }
        return this.helper(b);
    }
    private void helper(int b) {}
}
interface Shape { double area(); }
enum Mode { A, B }
record Point(int x, int y) {}
"#;
    const CS_SRC: &str = r#"using System;
using System.Collections.Generic;
using static System.Math;

namespace Acme.App
{
    public class Widget : Base
    {
        private int n;
        public Widget(int n) { this.n = n; }
        public int Add(int a, int b)
        {
            if (a > 0 && b > 0) return Util.Sum(a, b);
            foreach (var i in Items) { Log(i); }
            switch (a) { case 1: return 1; default: break; }
            return Helper(b);
        }
        private void Helper(int b) { }
    }
    public interface IShape { double Area(); }
    public struct Pt { public int X; }
    enum Mode { A, B }
    public record Point(int X, int Y);
}
namespace Acme.Other;
class Other { }
"#;
    const C_SRC: &str = r#"#include <stdio.h>
#include "util.h"
#define CAP 10

static int count;
struct widget { int n; };
typedef struct widget widget_t;
enum mode { A, B };

static int add(int a, int b) {
    if (a > 0 && b > 0) { return sum(a, b); }
    for (int i = 0; i < a; i++) { printf("%d", i); }
    switch (a) { case 1: return 1; default: break; }
    return b;
}

int main(void) { return add(1, 2); }
"#;
    const CPP_SRC: &str = r#"#include <vector>
#include "widget.hpp"

namespace acme {
class Widget : public Base {
public:
    Widget(int n) : n_(n) {}
    int add(int a, int b) {
        if (a > 0 && b > 0) return util::sum(a, b);
        for (auto i : items) { log(i); }
        return this->helper(b);
    }
private:
    int helper(int b);
    int n_;
};
int Widget::helper(int b) { return b; }
struct Pt { int x; };
template <typename T> T twice(T v) { return v * 2; }
}
int main() { return acme::twice(1); }
"#;
    const PHP_SRC: &str = r#"<?php
namespace Acme\App;

use Acme\Util\Helper;
use Acme\Util\{Sum, Log};
require_once __DIR__ . '/bootstrap.php';
include 'config.php';

const CAP = 10;

function add(int $a, int $b): int {
    if ($a > 0 && $b > 0) { return Helper::sum($a, $b); }
    foreach ($items as $i) { log_it($i); }
    switch ($a) { case 1: return 1; default: break; }
    return $b;
}

class Widget extends Base {
    private int $n;
    public function __construct(int $n) { $this->n = $n; }
    public function render(): string { return $this->helper(); }
    private function helper() { return ''; }
}
interface Shape { public function area(): float; }
trait Loggable { public function log() {} }
enum Mode { case A; case B; }
"#;
    const RB_SRC: &str = r#"require 'json'
require_relative 'util/helper'
load 'x.rb'

CAP = 10

def add(a, b)
  if a > 0 && b > 0
    return Helper.sum(a, b)
  end
  a.times { |i| log(i) }
  case a
  when 1 then 1
  else b
  end
  b unless a.nil?
end

class Widget < Base
  include Comparable
  def initialize(n); @n = n; end
  def render
    helper
  end
  def self.build; new(1); end
  private
  def helper; ''; end
end

module Acme
  def self.run; add(1, 2); end
end
"#;
    const KT_SRC: &str = r#"package com.acme

import java.util.List
import com.acme.util.sum

const val CAP = 10
val count: Int = 0

fun add(a: Int, b: Int): Int {
    if (a > 0 && b > 0) { return sum(a, b) }
    for (i in 0 until a) { log(i) }
    when (a) {
        1 -> return 1
        else -> {}
    }
    return b
}

class Widget(private val n: Int) : Base() {
    fun render(): String = Util.helper(n)
    private fun helper(): String = ""
    companion object {
        fun build(): Widget = Widget(1)
    }
}
interface Shape {
    fun area(): Double
}
data class Point(val x: Int, val y: Int)
object Registry {
    fun get() = 1
}
enum class Mode { A, B }
typealias Handler = (Int) -> Unit
"#;
    const SWIFT_SRC: &str = r#"import Foundation
import UIKit

let cap = 10
var count = 0

func add(_ a: Int, _ b: Int) -> Int {
    if a > 0 && b > 0 { return Util.sum(a, b) }
    for i in 0..<a { log(i) }
    switch a { case 1: return 1; default: break }
    guard a > 0 else { return 0 }
    return b
}

class Widget: Base {
    private var n: Int
    init(n: Int) { self.n = n }
    func render() -> String { return self.helper() }
    private func helper() -> String { "" }
    static func build() -> Widget { Widget(n: 1) }
}
struct Point { var x: Int }
protocol Shape { func area() -> Double }
enum Mode { case a, b }
extension Widget { func extra() {} }
typealias Handler = (Int) -> Void
"#;
    const SCALA_SRC: &str = r#"package com.acme

import java.util.List
import com.acme.util.{sum, log}

object Registry {
  val cap = 10
  def get(): Int = 1
}

def add(a: Int, b: Int): Int = {
  if (a > 0 && b > 0) return sum(a, b)
  for (i <- 0 until a) log(i)
  a match {
    case 1 => 1
    case _ => b
  }
}

class Widget(n: Int) extends Base {
  def render(): String = helper()
  private def helper(): String = ""
}
trait Shape { def area(): Double }
case class Point(x: Int, y: Int)
type Handler = Int => Unit
"#;
    const LUA_SRC: &str = r#"local json = require("json")
local util = require "acme.util"
require("side_effect")

CAP = 10
local count = 0

function add(a, b)
  if a > 0 and b > 0 then
    return util.sum(a, b)
  elseif a < 0 then
    return 0
  end
  for i = 1, a do log(i) end
  while false do end
  return b
end

local function helper() return "" end
local mul = function(a, b) return a * b end

local Widget = {}
function Widget.new(n) return setmetatable({ n = n }, Widget) end
function Widget:render() return self:helper() end

add(1, 2)
"#;
    const EX_SRC: &str = r#"defmodule Acme.Widget do
  use GenServer
  import Enum, only: [map: 2]
  alias Acme.Util.Helper
  require Logger

  @cap 10

  def add(a, b) when a > 0 do
    if a > 0 and b > 0 do
      Helper.sum(a, b)
    else
      b
    end
    case a do
      1 -> 1
      _ -> b
    end
    cond do
      a -> 1
      true -> 2
    end
  end

  defp helper(x), do: log(x)
  defmacro build(n), do: quote(do: unquote(n))

  def render(%{n: n}) do
    Logger.info("x")
    helper(n)
  end
end

defmodule Acme.Other do
  def run, do: Acme.Widget.add(1, 2)
end
"#;
    const SH_SRC: &str = r#"#!/usr/bin/env bash
set -euo pipefail
source ./lib/util.sh
. "$HOME/.config/env"

CAP=10

add() {
  local a=$1 b=$2
  if [[ $a -gt 0 && $b -gt 0 ]]; then
    sum "$a" "$b"
  elif [[ $a -lt 0 ]]; then
    return 1
  fi
  for i in $(seq 1 "$a"); do log "$i"; done
  while false; do :; done
  case $a in
    1) echo one ;;
    *) echo other ;;
  esac
  [[ -n $a ]] || return 1
}

function helper {
  echo ""
}

add 1 2
main "$@"
"#;
}

// ── React reading (2026-09-12): components, hooks, wrappers, import kinds, JSX renders ──
//
// The audit case: a React `components/` folder where `const Message = forwardRef(…)`,
// `export const Icon = memo(…)` and `export const useStore = Ctx.useContext` produced NO def, so
// the judge called the files "glue" and scored them 100, and the paper called a UI folder "a
// procedural library" because nothing was ever a `component`.
#[cfg(test)]
mod react_tests {
    use super::*;

    const SRC: &str = r#"import type {X} from './types';
import {type A, type B} from './ab';
import {type Y, Z} from './mixed';
import React, {memo, forwardRef} from 'react';
export {default as Editor} from './Editor';
export * from './all';
export type {T2} from './t2';
export {A, B as C} from './ab';
const Lazy = dynamic(() => import('./Impl'), {ssr: false});
export default function Header(): JSX.Element { return <div><Row a={1}/><Row a={2}/><ui.Button/></div>; }
const Message = forwardRef<HTMLDivElement, P>((props, ref) => <div ref={ref}/>);
export const Icon = memo<P>(function Icon(props) { return <svg/>; });
export const useStore = Ctx.useContext;
const Wrapped = memo(Inner);
export const Styled = styled.div`color: red`;
class Store extends React.Component<P> { render() { return <p/>; } }
class Bag extends Map<string, number> {}
type T = 1;
interface I { a: number }
enum E { A, B }
export const CAP = 10;
function useThing() { if (x) { return 1; } return 2; }
function helper(a) { return a; }
"#;

    fn parsed() -> Value {
        parseTreeSitter(&json!(SRC), &json!("a.tsx"), &json!("tsx"))
    }
    fn node(v: &Value, name: &str) -> Value {
        v["nodes"].as_array().unwrap().iter().find(|n| n["name"] == json!(name)).cloned().unwrap_or_else(|| panic!("no def {name}: {:?}", v["nodes"].as_array().unwrap().iter().map(|n| n["name"].clone()).collect::<Vec<_>>()))
    }
    fn has(v: &Value, name: &str) -> bool {
        v["nodes"].as_array().unwrap().iter().any(|n| n["name"] == json!(name))
    }

    #[test]
    fn components_hooks_and_wrappers_are_recognised() {
        let v = parsed();
        assert_eq!(v["has_error"], json!(false));
        assert_eq!(node(&v, "Header")["type"], json!("component"), "PascalCase + JSX");
        assert_eq!(node(&v, "Header")["exported"], json!(true));
        assert_eq!(node(&v, "Message")["type"], json!("component"), "forwardRef((props, ref) => …)");
        assert_eq!(node(&v, "Message")["signature"], json!("(props, ref)"), "signature from the wrapped fn");
        assert_eq!(node(&v, "Icon")["type"], json!("component"), "memo(function Icon …)");
        assert_eq!(node(&v, "Icon")["signature"], json!("(props)"));
        assert_eq!(node(&v, "useStore")["type"], json!("hook"), "hook alias");
        assert_eq!(node(&v, "useStore")["signature"], json!("= Ctx.useContext"));
        assert_eq!(node(&v, "Wrapped")["type"], json!("component"), "memo(Inner)");
        assert!(node(&v, "Wrapped")["span"].get("complexity").is_none(), "an alias has no body to score");
        assert_eq!(node(&v, "Store")["type"], json!("component"), "extends React.Component");
        assert_eq!(node(&v, "Bag")["type"], json!("class"));
        assert_eq!(node(&v, "useThing")["type"], json!("hook"));
        assert_eq!(node(&v, "useThing")["span"]["complexity"], json!(2), "hooks are code paths");
        assert_eq!(node(&v, "helper")["type"], json!("function"));
        assert_eq!(node(&v, "T")["type"], json!("type"));
        assert_eq!(node(&v, "I")["type"], json!("type"));
        assert_eq!(node(&v, "E")["type"], json!("type"));
        assert_eq!(node(&v, "CAP")["type"], json!("const"));
        assert!(!has(&v, "Styled"), "styled.div`…` is not a definition");
        assert!(!has(&v, "Lazy"), "dynamic(…) is not a definition (its import IS an edge)");
    }

    #[test]
    fn import_kinds_type_reexport_dynamic() {
        let v = parsed();
        let imports: Vec<(String, String)> = v["imports"].as_array().unwrap().iter().map(|i| (i["specifier"].as_str().unwrap().to_string(), i["kind"].as_str().unwrap().to_string())).collect();
        let expect = vec![
            ("./types", "type"), ("./ab", "type"), ("./mixed", "value"), ("react", "value"),
            ("./Editor", "reexport"), ("./all", "reexport"), ("./t2", "type"), ("./ab", "reexport"),
            ("./Impl", "dynamic"),
        ];
        let expect: Vec<(String, String)> = expect.into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        assert_eq!(imports, expect);
    }

    #[test]
    fn jsx_usages_are_renders_edges_once_per_child() {
        let v = parsed();
        let renders: Vec<(String, String)> = v["calls"].as_array().unwrap().iter().filter(|c| c["via"] == json!("jsx")).map(|c| (c["caller"].as_str().unwrap().to_string(), c["callee"].as_str().unwrap().to_string())).collect();
        assert_eq!(renders, vec![("Header".to_string(), "Row".to_string()), ("Header".to_string(), "Button".to_string())], "<Row/> twice = one edge; <ui.Button/> → Button; <div/> is not a component");
    }

    #[test]
    fn constructing_a_class_is_a_call() {
        let v = parseTreeSitter(&json!("export function run(fn) { visit(fn, new Transform(), false); }\nclass Transform extends Base {}\n"), &json!("p.ts"), &json!("typescript"));
        let calls: Vec<(&str, &str)> = v["calls"].as_array().unwrap().iter().map(|c| (c["caller"].as_str().unwrap(), c["callee"].as_str().unwrap())).collect();
        assert!(calls.contains(&("run", "Transform")), "{calls:?}");
        assert!(calls.contains(&("run", "visit")));
    }

    #[test]
    fn definitions_passed_by_name_are_refs_not_calls() {
        let v = parseTreeSitter(&json!("function reducer(s, a) { return s; }\nexport function Provider() { const [s] = useReducer(reducer, init); return <p/>; }\n"), &json!("s.tsx"), &json!("tsx"));
        let calls: Vec<&str> = v["calls"].as_array().unwrap().iter().map(|c| c["callee"].as_str().unwrap()).collect();
        assert_eq!(calls, vec!["useReducer"], "the call list is unchanged");
        let refs: Vec<(&str, &str)> = v["refs"].as_array().unwrap().iter().map(|c| (c["caller"].as_str().unwrap(), c["callee"].as_str().unwrap())).collect();
        assert_eq!(refs, vec![("Provider", "reducer"), ("Provider", "init")], "both names are candidates; binding-resolve keeps the one that is a definition");
    }

    #[test]
    fn file_node_carries_the_files_length() {
        let v = parsed();
        assert_eq!(v["nodes"][0]["span"]["end_line"], json!(SRC.lines().count()));
    }

    #[test]
    fn kotlin_style_wrapper_on_a_lowercase_name_is_still_a_function() {
        let v = parseTreeSitter(&json!("const handler = memo(() => 1);\n"), &json!("h.ts"), &json!("typescript"));
        assert_eq!(node(&v, "handler")["type"], json!("function"), "a wrapper does not make a lowercase name a component");
    }
}
