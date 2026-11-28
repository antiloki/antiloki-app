//! lang-profiles.rs — walk profiles for the grammars that parse-treesitter.rs did not hand-write
//! (go, java, c_sharp, c, cpp, php, ruby, kotlin, swift, scala, lua, elixir, bash).
//!
//! NO JS TWIN. The JS lane had `goWalk`/`exWalk`/`bashWalk`/`luaWalk` plus a `genericWalk`; that
//! lane is deleted and nothing here is a port. The contract is the atlas's own — the same
//! `{nodes, imports, calls}` shape `ts_walk`/`py_walk`/`rust_walk` produce, so every consumer
//! (dep-resolve, atlas-build, code-decorate, the constitution) sees these languages exactly the
//! way it already sees TypeScript.
//!
//! ## One walker, thirteen specs
//! Thirteen hand-rolled walkers would be thirteen copies of the same loop. Instead a `Profile`
//! answers the questions the loop asks — *is this node a definition, and of what kind? what is
//! its name / parameters / body? is it an import, and of what? is it a call, of what? does it
//! branch?* — and `walk` does the rest. Every node-kind string below was read off a real parse
//! (`tree.root_node().to_sexp()` of a sample file), not guessed from grammar docs.
//!
//! ## Rules shared with the hand-written profiles
//! - Functions are not walked INTO for definitions (a nested fn is not a top-level symbol); calls
//!   inside them are attributed to the enclosing definition as `caller`.
//! - Type bodies ARE walked, and a function found there is a `method` (the Rust-profile reading of
//!   `impl`), except where the language's own vocabulary says otherwise (`methods_in_types`).
//! - `exported` means "visible outside this file" in the language's own terms: Go's capital
//!   letter, Java/C#'s `public`, Ruby's `private` section, C++'s access specifiers, Lua's `local`.
//! - Complexity is base 1 + one per branch; `&&`/`||`/`and`/`or` count. Branch kinds are listed
//!   explicitly per language rather than regex-matched on kind NAME — the phantom-count trap the
//!   Rust profile documents (`match_expression` + `match_arm` both matching) exists in every
//!   language with a switch: Java's `switch_expression`, Kotlin's `when_expression`, Elixir's
//!   `case`. Arms count, the statement wrapping them does not.
//!
//! ## Known grammar limits (documented, not hidden)
//! - kotlin-ng 1.1.0 wants a newline or `;` before a closing `}` on the same line as a member:
//!   `interface Shape { fun area(): Double }` parses with a MISSING node. The definitions are
//!   still extracted; the file is flagged `has_error` → atlas health "degraded", which is the
//!   honest reading.
use serde_json::{json, Value};
use tree_sitter::Node;

use crate::codebase_parse_treesitter::{add_call, add_import, is_bool_op, named_children, span, strip_quotes, text, Out};

/// What a node is, in the atlas's def vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Def {
    Function,
    /// A function the language itself marks as belonging to a type even outside its body — Go's
    /// receiver, C++'s `Widget::helper`, Lua's `Widget:render`. `Function` inside a type body
    /// becomes a method anyway; this is for the ones declared elsewhere.
    Method,
    Class,
    Const,
    Type,
}

impl Def {
    fn label(self, in_type: bool, methods_in_types: bool) -> &'static str {
        match self {
            Def::Function => if in_type && methods_in_types { "method" } else { "function" },
            Def::Method => "method",
            Def::Class => "class",
            Def::Const => "const",
            Def::Type => "type",
        }
    }
}

/// How to enter a node that is not a definition (see `Profile::is_container`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Descend {
    No,
    Same,
    AsType,
}

/// Everything the walker asks a language. Each fn gets the node and the source bytes.
pub(crate) struct Profile {
    /// Definition kind for this node, or None. `in_type` is true inside a class/struct/trait body,
    /// so a language can e.g. register fields only at file scope.
    pub def_kind: fn(Node, &[u8], bool) -> Option<Def>,
    /// The defined name. None means "anonymous — not a symbol".
    pub name_of: fn(Node, &[u8]) -> Option<String>,
    /// The parameter list text, `()` when there is none.
    pub signature: fn(Node, &[u8]) -> String,
    /// The node whose calls belong to this definition (function body / class body).
    pub body_of: fn(Node) -> Option<Node>,
    /// Descend into this node WITHOUT registering it — a namespace, an `#ifdef` block, a
    /// declaration list (`Same` scope), or a type-shaped body that is not itself a new symbol,
    /// like a Swift `extension` (`AsType`, so what it holds are methods).
    pub is_container: fn(Node, &[u8]) -> Descend,
    /// Import specifiers carried by this node — empty when it is not an import.
    pub imports: fn(Node, &[u8]) -> Vec<String>,
    /// The callee name if this node is a call site.
    pub callee: fn(Node, &[u8]) -> Option<String>,
    /// Is this definition visible outside the file? Overridden by a visibility section (below).
    pub exported: fn(Node, &[u8], &str) -> bool,
    /// Visibility state a type body STARTS in (`Some(false)` for a C++ class), or None to ask
    /// `exported` per member.
    pub type_visibility: fn(Node, &[u8]) -> Option<bool>,
    /// A statement that flips visibility for what FOLLOWS it (Ruby `private`, C++ `public:`).
    pub visibility_marker: fn(Node, &[u8]) -> Option<bool>,
    /// Does this node add a branch?
    pub is_decision: fn(Node, &[u8]) -> bool,
    /// Kinds whose direct children are checked for `&&`/`||`/`and`/`or`.
    pub bool_op_hosts: &'static [&'static str],
    /// A function inside a type body is a `method` (true) or stays a `function` (Elixir).
    pub methods_in_types: bool,
}

// ── generic helpers ──────────────────────────────────────────────────────────────────────────

fn field<'t>(n: Node<'t>, name: &str) -> Option<Node<'t>> {
    n.child_by_field_name(name)
}

fn field_text(n: Node, name: &str, src: &[u8]) -> Option<String> {
    field(n, name).map(|c| text(c, src))
}

fn child_of_kind<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    named_children(n).into_iter().find(|c| c.kind() == kind)
}

fn child_of_kinds<'t>(n: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    named_children(n).into_iter().find(|c| kinds.contains(&c.kind()))
}

/// Any child (named or anonymous) whose text equals `t`.
fn has_token(n: Node, src: &[u8], t: &str) -> bool {
    let mut c = n.walk();
    let hit = n.children(&mut c).any(|ch| text(ch, src) == t);
    hit
}

fn first_token(n: Node, src: &[u8]) -> String {
    n.child(0).map(|c| text(c, src)).unwrap_or_default()
}

/// Last identifier-like descendant in document order — the member name of `a.b.c()` for every
/// grammar that spells navigation as nested expression nodes.
fn last_ident(n: Node, src: &[u8], kinds: &[&str]) -> Option<String> {
    // depth-first, left-to-right; the LAST hit wins
    fn visit(n: Node, src: &[u8], kinds: &[&str], found: &mut Option<String>) {
        if kinds.contains(&n.kind()) {
            *found = Some(text(n, src));
        }
        for c in named_children(n) {
            visit(c, src, kinds, found);
        }
    }
    let mut found: Option<String> = None;
    visit(n, src, kinds, &mut found);
    found
}

/// Descendants of a given kind, depth-first.
fn descendants<'t>(n: Node<'t>, kind: &str, out: &mut Vec<Node<'t>>) {
    if n.kind() == kind {
        out.push(n);
    }
    for c in named_children(n) {
        descendants(c, kind, out);
    }
}

fn params_field(n: Node, src: &[u8]) -> String {
    field_text(n, "parameters", src).unwrap_or_else(|| "()".to_string())
}

fn modifiers_have(n: Node, src: &[u8], modifier_kinds: &[&str], word: &str) -> bool {
    named_children(n).iter().any(|c| modifier_kinds.contains(&c.kind()) && text(*c, src).split_whitespace().any(|w| w == word))
}

fn no_marker(_: Node, _: &[u8]) -> Option<bool> {
    None
}
fn always_exported(_: Node, _: &[u8], _: &str) -> bool {
    true
}
fn not_container(_: Node, _: &[u8]) -> Descend {
    Descend::No
}
fn same_if(n: Node, kinds: &[&str]) -> Descend {
    if kinds.contains(&n.kind()) { Descend::Same } else { Descend::No }
}
fn body_field(n: Node) -> Option<Node> {
    field(n, "body")
}

// ── the walker ───────────────────────────────────────────────────────────────────────────────

fn complexity(p: &Profile, decl: Node, src: &[u8]) -> i64 {
    let mut n: i64 = 1;
    let mut stack: Vec<Node> = vec![decl];
    while let Some(node) = stack.pop() {
        if node != decl && (p.is_decision)(node, src) {
            n += 1;
        }
        if p.bool_op_hosts.contains(&node.kind()) {
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                if is_bool_op(&text(ch, src)) {
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

fn collect_calls<'a>(p: &Profile, node: Node<'a>, src: &[u8], found: &mut Vec<(String, Node<'a>)>) {
    if let Some(name) = (p.callee)(node, src) {
        found.push((name, node));
    }
    for c in named_children(node) {
        collect_calls(p, c, src, found);
    }
}

fn record_calls(p: &Profile, body: Option<Node>, caller: Option<&str>, path: &str, src: &[u8], out: &mut Out) {
    let Some(body) = body else { return };
    let mut found: Vec<(String, Node)> = Vec::new();
    collect_calls(p, body, src, &mut found);
    for (name, node) in found {
        add_call(out, path, caller, name, node);
    }
}

#[allow(clippy::too_many_arguments)]
fn push_def(out: &mut Out, path: &str, kind: &str, name: &str, node: Node, signature: String, exported: bool, complexity: Option<i64>) {
    let mut sp = span(path, node);
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

struct Scope {
    in_type: bool,
    /// Section visibility (Ruby `private`, C++ `public:`); None → ask the profile per def.
    vis: Option<bool>,
}

pub(crate) fn walk(p: &Profile, root: Node, path: &str, src: &[u8], out: &mut Out) {
    walk_in(p, root, path, src, out, &mut Scope { in_type: false, vis: None });
}

fn walk_in(p: &Profile, parent: Node, path: &str, src: &[u8], out: &mut Out, scope: &mut Scope) {
    for node in named_children(parent) {
        // A grammar hiccup wraps whatever it could not place in an ERROR node; the definitions
        // inside are still real. Walk through it rather than lose a whole class to one bad line
        // (the file is flagged `has_error` regardless — see the module doc's kotlin-ng note).
        if node.kind() == "ERROR" {
            walk_in(p, node, path, src, out, scope);
            continue;
        }
        if let Some(v) = (p.visibility_marker)(node, src) {
            scope.vis = Some(v);
            continue;
        }
        let specs = (p.imports)(node, src);
        if !specs.is_empty() {
            for s in specs {
                add_import(out, path, s, node);
            }
            continue;
        }
        if let Some(def) = (p.def_kind)(node, src, scope.in_type) {
            if let Some(name) = (p.name_of)(node, src) {
                let label = def.label(scope.in_type, p.methods_in_types);
                let exported = scope.vis.unwrap_or_else(|| (p.exported)(node, src, &name));
                match def {
                    Def::Function | Def::Method => {
                        push_def(out, path, label, &name, node, (p.signature)(node, src), exported, Some(complexity(p, node, src)));
                        record_calls(p, (p.body_of)(node), Some(&name), path, src, out);
                    }
                    Def::Class => {
                        push_def(out, path, label, &name, node, (p.signature)(node, src), exported, None);
                        if let Some(body) = (p.body_of)(node) {
                            let mut inner = Scope { in_type: true, vis: (p.type_visibility)(node, src) };
                            walk_in(p, body, path, src, out, &mut inner);
                        }
                    }
                    Def::Const | Def::Type => {
                        push_def(out, path, label, &name, node, "()".to_string(), exported, None);
                    }
                }
                continue;
            }
        }
        match (p.is_container)(node, src) {
            Descend::Same => {
                walk_in(p, node, path, src, out, scope);
                continue;
            }
            Descend::AsType => {
                let mut inner = Scope { in_type: true, vis: (p.type_visibility)(node, src) };
                let body = (p.body_of)(node).unwrap_or(node);
                walk_in(p, body, path, src, out, &mut inner);
                continue;
            }
            Descend::No => {}
        }
        record_calls(p, Some(node), None, path, src, out);
    }
}

// ═════════════════════════════════════════════════════════════════════════════════════════════
// GO
// ═════════════════════════════════════════════════════════════════════════════════════════════

fn go_def(n: Node, _: &[u8], _: bool) -> Option<Def> {
    match n.kind() {
        "function_declaration" => Some(Def::Function),
        "method_declaration" => Some(Def::Method),
        "type_spec" => Some(match field(n, "type").map(|t| t.kind()) {
            Some("struct_type") | Some("interface_type") => Def::Class,
            _ => Def::Type,
        }),
        "const_spec" | "var_spec" => Some(Def::Const),
        _ => None,
    }
}
fn go_name(n: Node, src: &[u8]) -> Option<String> {
    field_text(n, "name", src)
}
fn go_imports(n: Node, src: &[u8]) -> Vec<String> {
    if n.kind() != "import_declaration" {
        return vec![];
    }
    let mut specs = Vec::new();
    descendants(n, "import_spec", &mut specs);
    specs.iter().filter_map(|s| field_text(*s, "path", src)).map(|t| strip_quotes(&t)).collect()
}
fn go_callee(n: Node, src: &[u8]) -> Option<String> {
    if n.kind() != "call_expression" {
        return None;
    }
    let f = field(n, "function")?;
    match f.kind() {
        "identifier" => Some(text(f, src)),
        "selector_expression" => field_text(f, "field", src),
        _ => None,
    }
}
fn go_exported(_: Node, _: &[u8], name: &str) -> bool {
    name.chars().next().map_or(false, |c| c.is_uppercase())
}
fn go_decision(n: Node, _: &[u8]) -> bool {
    matches!(n.kind(), "if_statement" | "for_statement" | "expression_case" | "type_case" | "default_case" | "communication_case")
}
pub(crate) static GO: Profile = Profile {
    def_kind: go_def,
    name_of: go_name,
    signature: params_field,
    body_of: body_field,
    is_container: |n, _| same_if(n, &["type_declaration", "const_declaration", "var_declaration"]),
    imports: go_imports,
    callee: go_callee,
    exported: go_exported,
    type_visibility: no_marker,
    visibility_marker: no_marker,
    is_decision: go_decision,
    bool_op_hosts: &["binary_expression"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// JAVA
// ═════════════════════════════════════════════════════════════════════════════════════════════

fn java_def(n: Node, _: &[u8], _: bool) -> Option<Def> {
    match n.kind() {
        "method_declaration" | "constructor_declaration" => Some(Def::Function),
        "class_declaration" | "interface_declaration" | "enum_declaration" | "record_declaration" | "annotation_type_declaration" => Some(Def::Class),
        _ => None,
    }
}
fn java_imports(n: Node, src: &[u8]) -> Vec<String> {
    if n.kind() != "import_declaration" {
        return vec![];
    }
    let Some(path) = child_of_kinds(n, &["scoped_identifier", "identifier"]) else { return vec![] };
    let mut spec = text(path, src);
    if has_token(n, src, "*") {
        spec.push_str(".*");
    }
    if has_token(n, src, "static") {
        spec = format!("static {spec}");
    }
    vec![spec]
}
fn java_callee(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "method_invocation" => field_text(n, "name", src),
        "object_creation_expression" => field(n, "type").map(|t| last_ident(t, src, &["type_identifier", "identifier"]).unwrap_or_else(|| text(t, src))),
        _ => None,
    }
}
fn java_exported(n: Node, src: &[u8], _: &str) -> bool {
    modifiers_have(n, src, &["modifiers"], "public")
}
fn java_decision(n: Node, src: &[u8]) -> bool {
    match n.kind() {
        "if_statement" | "for_statement" | "enhanced_for_statement" | "while_statement" | "do_statement" | "catch_clause" | "ternary_expression" => true,
        "switch_label" | "switch_rule" => !text(n, src).starts_with("default"),
        _ => false,
    }
}
pub(crate) static JAVA: Profile = Profile {
    def_kind: java_def,
    name_of: |n, src| field_text(n, "name", src),
    signature: params_field,
    body_of: body_field,
    is_container: not_container,
    imports: java_imports,
    callee: java_callee,
    exported: java_exported,
    // interface members are implicitly public
    type_visibility: |n, _| if n.kind() == "interface_declaration" { Some(true) } else { None },
    visibility_marker: no_marker,
    is_decision: java_decision,
    bool_op_hosts: &["binary_expression"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// C#
// ═════════════════════════════════════════════════════════════════════════════════════════════

fn cs_def(n: Node, _: &[u8], _: bool) -> Option<Def> {
    match n.kind() {
        "method_declaration" | "constructor_declaration" | "local_function_statement" => Some(Def::Function),
        "class_declaration" | "interface_declaration" | "struct_declaration" | "enum_declaration" | "record_declaration" | "record_struct_declaration" => Some(Def::Class),
        "delegate_declaration" => Some(Def::Type),
        _ => None,
    }
}
fn cs_imports(n: Node, src: &[u8]) -> Vec<String> {
    if n.kind() != "using_directive" {
        return vec![];
    }
    // `using static System.Math;` / `using Foo = Bar.Baz;` / `global using X;` — resolve_csharp
    // already understands the `static ` prefix and the alias `=`, so hand it the directive text.
    let t = text(n, src);
    let t = t.trim().trim_start_matches("global").trim().trim_start_matches("using").trim().trim_end_matches(';').trim();
    if t.is_empty() { vec![] } else { vec![t.to_string()] }
}
fn cs_callee(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "invocation_expression" => {
            let f = field(n, "function")?;
            match f.kind() {
                "identifier" => Some(text(f, src)),
                "member_access_expression" => field(f, "name").map(|x| last_ident(x, src, &["identifier"]).unwrap_or_else(|| text(x, src))),
                "generic_name" => last_ident(f, src, &["identifier"]),
                _ => None,
            }
        }
        "object_creation_expression" => field(n, "type").and_then(|t| last_ident(t, src, &["identifier"])),
        _ => None,
    }
}
fn cs_exported(n: Node, src: &[u8], _: &str) -> bool {
    modifiers_have(n, src, &["modifier"], "public")
}
fn cs_decision(n: Node, _: &[u8]) -> bool {
    match n.kind() {
        "if_statement" | "for_statement" | "foreach_statement" | "while_statement" | "do_statement" | "catch_clause" | "conditional_expression" | "switch_expression_arm" => true,
        // a `default:` section carries no pattern
        "switch_section" => named_children(n).iter().any(|c| c.kind().ends_with("_pattern") || c.kind() == "case_switch_label"),
        _ => false,
    }
}
pub(crate) static C_SHARP: Profile = Profile {
    def_kind: cs_def,
    name_of: |n, src| field_text(n, "name", src),
    signature: params_field,
    body_of: body_field,
    is_container: |n, _| same_if(n, &["namespace_declaration", "declaration_list"]),
    imports: cs_imports,
    callee: cs_callee,
    exported: cs_exported,
    // interface members are implicitly public
    type_visibility: |n, _| if n.kind() == "interface_declaration" { Some(true) } else { None },
    visibility_marker: no_marker,
    is_decision: cs_decision,
    bool_op_hosts: &["binary_expression"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// C / C++ (one profile; the C++ kinds are a superset)
// ═════════════════════════════════════════════════════════════════════════════════════════════

/// `int *Widget::helper(int b)` → the declarator chain bottoms out at the name.
fn c_declarator_name(n: Node, src: &[u8]) -> Option<String> {
    let mut cur = field(n, "declarator")?;
    loop {
        match cur.kind() {
            "function_declarator" | "pointer_declarator" | "reference_declarator" | "parenthesized_declarator" | "init_declarator" | "array_declarator" => {
                cur = field(cur, "declarator").or_else(|| named_children(cur).into_iter().next())?;
            }
            "qualified_identifier" => return field_text(cur, "name", src).or_else(|| Some(text(cur, src))),
            "identifier" | "field_identifier" | "type_identifier" | "destructor_name" | "operator_name" => return Some(text(cur, src)),
            _ => return None,
        }
    }
}
fn c_def(n: Node, _: &[u8], _: bool) -> Option<Def> {
    match n.kind() {
        // `int Widget::helper(int b) { … }` — defined outside the class body, still a method
        "function_definition" if c_function_declarator(n).and_then(|f| field(f, "declarator")).map_or(false, |d| d.kind() == "qualified_identifier") => Some(Def::Method),
        "function_definition" | "preproc_function_def" => Some(Def::Function),
        "struct_specifier" | "union_specifier" | "enum_specifier" | "class_specifier" => {
            // a forward declaration (`struct foo;`) has no body and is not a definition
            if field(n, "body").is_some() { Some(Def::Class) } else { None }
        }
        "type_definition" | "alias_declaration" => Some(Def::Type),
        "preproc_def" => Some(Def::Const),
        _ => None,
    }
}
fn c_name(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "function_definition" | "type_definition" => c_declarator_name(n, src),
        _ => field_text(n, "name", src),
    }
}
/// The `function_declarator` on this definition's own declarator chain (not one nested in a body).
fn c_function_declarator(n: Node) -> Option<Node> {
    let mut cur = field(n, "declarator")?;
    loop {
        match cur.kind() {
            "function_declarator" => return Some(cur),
            "pointer_declarator" | "reference_declarator" | "parenthesized_declarator" => {
                cur = field(cur, "declarator").or_else(|| named_children(cur).into_iter().next())?;
            }
            _ => return None,
        }
    }
}
fn c_signature(n: Node, src: &[u8]) -> String {
    c_function_declarator(n).and_then(|f| field_text(f, "parameters", src)).unwrap_or_else(|| "()".to_string())
}
fn c_imports(n: Node, src: &[u8]) -> Vec<String> {
    if n.kind() != "preproc_include" {
        return vec![];
    }
    // `"util.h"` → util.h (relative, resolvable); `<stdio.h>` keeps its brackets so resolve_c
    // can recognise a system header.
    match field(n, "path") {
        Some(p) if p.kind() == "string_literal" => vec![strip_quotes(&text(p, src))],
        Some(p) => vec![text(p, src)],
        None => vec![],
    }
}
fn c_callee(n: Node, src: &[u8]) -> Option<String> {
    if n.kind() != "call_expression" {
        return None;
    }
    let f = field(n, "function")?;
    match f.kind() {
        "identifier" => Some(text(f, src)),
        "field_expression" => field_text(f, "field", src),
        "qualified_identifier" | "template_function" => last_ident(f, src, &["identifier"]),
        _ => None,
    }
}
fn c_exported(n: Node, src: &[u8], _: &str) -> bool {
    // `static` is file-local; everything else has external linkage. An out-of-line member
    // (`int Widget::helper()`) is as visible as its class — call it exported.
    !named_children(n).iter().any(|c| c.kind() == "storage_class_specifier" && text(*c, src) == "static")
}
fn c_type_visibility(n: Node, _: &[u8]) -> Option<bool> {
    match n.kind() {
        "class_specifier" => Some(false),
        "struct_specifier" | "union_specifier" => Some(true),
        _ => None,
    }
}
fn c_marker(n: Node, src: &[u8]) -> Option<bool> {
    if n.kind() == "access_specifier" { Some(text(n, src).starts_with("public")) } else { None }
}
fn c_decision(n: Node, src: &[u8]) -> bool {
    match n.kind() {
        "if_statement" | "for_statement" | "for_range_loop" | "while_statement" | "do_statement" | "conditional_expression" | "catch_clause" => true,
        "case_statement" => !text(n, src).starts_with("default"),
        _ => false,
    }
}
pub(crate) static C: Profile = Profile {
    def_kind: c_def,
    name_of: c_name,
    signature: c_signature,
    body_of: body_field,
    is_container: |n, _| same_if(n, &["preproc_ifdef", "preproc_if", "preproc_else", "preproc_elif", "preproc_elifdef", "linkage_specification", "declaration_list", "namespace_definition", "template_declaration"]),
    imports: c_imports,
    callee: c_callee,
    exported: c_exported,
    type_visibility: c_type_visibility,
    visibility_marker: c_marker,
    is_decision: c_decision,
    bool_op_hosts: &["binary_expression"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// PHP
// ═════════════════════════════════════════════════════════════════════════════════════════════

fn php_def(n: Node, _: &[u8], _: bool) -> Option<Def> {
    match n.kind() {
        "function_definition" | "method_declaration" => Some(Def::Function),
        "class_declaration" | "interface_declaration" | "trait_declaration" | "enum_declaration" => Some(Def::Class),
        "const_declaration" => Some(Def::Const),
        _ => None,
    }
}
fn php_name(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "const_declaration" => child_of_kind(n, "const_element").and_then(|e| child_of_kind(e, "name")).map(|x| text(x, src)),
        _ => field_text(n, "name", src),
    }
}
/// The string inside `require __DIR__ . '/x.php'` / `include 'y.php'` — the path part only.
fn php_require_target(n: Node, src: &[u8]) -> Option<String> {
    let mut strings = Vec::new();
    descendants(n, "string", &mut strings);
    descendants(n, "encapsed_string", &mut strings);
    let s = strings.last()?;
    let raw = strip_quotes(&text(*s, src));
    let uses_dir = text(n, src).contains("__DIR__");
    Some(if uses_dir { raw.trim_start_matches('/').to_string() } else { raw })
}
fn php_imports(n: Node, src: &[u8]) -> Vec<String> {
    match n.kind() {
        "namespace_use_declaration" => {
            // `use A\B\C;` and the group form `use A\B\{C, D};`
            let prefix = child_of_kind(n, "namespace_name").map(|p| text(p, src));
            let mut clauses = Vec::new();
            descendants(n, "namespace_use_clause", &mut clauses);
            clauses
                .iter()
                .filter_map(|c| named_children(*c).into_iter().next().map(|x| text(x, src)))
                .map(|name| match &prefix {
                    Some(p) if field(n, "body").is_some() => format!("{p}\\{name}"),
                    _ => name,
                })
                .collect()
        }
        "expression_statement" => {
            let inner = named_children(n).into_iter().next();
            match inner.map(|i| i.kind()) {
                Some("require_expression") | Some("require_once_expression") | Some("include_expression") | Some("include_once_expression") => {
                    php_require_target(inner.unwrap(), src).into_iter().collect()
                }
                _ => vec![],
            }
        }
        _ => vec![],
    }
}
fn php_callee(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "function_call_expression" => field(n, "function").and_then(|f| if f.kind() == "name" || f.kind() == "qualified_name" { last_ident(f, src, &["name"]) } else { None }),
        "scoped_call_expression" | "member_call_expression" | "nullsafe_member_call_expression" => field_text(n, "name", src),
        "object_creation_expression" => child_of_kinds(n, &["name", "qualified_name"]).and_then(|t| last_ident(t, src, &["name"])),
        _ => None,
    }
}
fn php_exported(n: Node, src: &[u8], _: &str) -> bool {
    // members default to public; only an explicit private/protected hides one
    !named_children(n).iter().any(|c| c.kind() == "visibility_modifier" && matches!(text(*c, src).as_str(), "private" | "protected"))
}
fn php_decision(n: Node, _: &[u8]) -> bool {
    matches!(
        n.kind(),
        "if_statement" | "else_if_clause" | "for_statement" | "foreach_statement" | "while_statement" | "do_statement" | "case_statement" | "catch_clause" | "conditional_expression" | "match_conditional_expression"
    )
}
pub(crate) static PHP: Profile = Profile {
    def_kind: php_def,
    name_of: php_name,
    signature: params_field,
    body_of: body_field,
    is_container: |n, _| same_if(n, &["namespace_definition", "compound_statement", "declaration_list"]),
    imports: php_imports,
    callee: php_callee,
    exported: php_exported,
    type_visibility: no_marker,
    visibility_marker: no_marker,
    is_decision: php_decision,
    bool_op_hosts: &["binary_expression"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// RUBY
// ═════════════════════════════════════════════════════════════════════════════════════════════

fn rb_def(n: Node, _: &[u8], _: bool) -> Option<Def> {
    match n.kind() {
        "method" | "singleton_method" => Some(Def::Function),
        "class" | "module" => Some(Def::Class),
        _ => None,
    }
}
fn rb_call_method(n: Node, src: &[u8]) -> Option<String> {
    field(n, "method").map(|m| text(m, src))
}
fn rb_imports(n: Node, src: &[u8]) -> Vec<String> {
    if n.kind() != "call" || field(n, "receiver").is_some() {
        return vec![];
    }
    match rb_call_method(n, src).as_deref() {
        Some("require") | Some("require_relative") | Some("load") => {
            let mut strings = Vec::new();
            if let Some(args) = field(n, "arguments") {
                descendants(args, "string", &mut strings);
            }
            strings.first().map(|s| strip_quotes(&text(*s, src))).into_iter().collect()
        }
        _ => vec![],
    }
}
fn rb_callee(n: Node, src: &[u8]) -> Option<String> {
    if n.kind() != "call" {
        return None;
    }
    rb_call_method(n, src)
}
fn rb_marker(n: Node, src: &[u8]) -> Option<bool> {
    if n.kind() != "identifier" {
        return None;
    }
    match text(n, src).as_str() {
        "private" | "protected" => Some(false),
        "public" => Some(true),
        _ => None,
    }
}
fn rb_decision(n: Node, _: &[u8]) -> bool {
    matches!(
        n.kind(),
        "if" | "unless" | "elsif" | "while" | "until" | "for" | "when" | "in_clause" | "rescue" | "conditional" | "if_modifier" | "unless_modifier" | "while_modifier" | "until_modifier" | "rescue_modifier"
    )
}
pub(crate) static RUBY: Profile = Profile {
    def_kind: rb_def,
    name_of: |n, src| field_text(n, "name", src),
    signature: params_field,
    body_of: body_field,
    // `class << self ... end` — its defs are methods of the enclosing class
    is_container: |n, _| if n.kind() == "singleton_class" { Descend::AsType } else { Descend::No },
    imports: rb_imports,
    callee: rb_callee,
    exported: always_exported,
    type_visibility: |_, _| Some(true),
    visibility_marker: rb_marker,
    is_decision: rb_decision,
    bool_op_hosts: &["binary"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// KOTLIN (kotlin-ng)
// ═════════════════════════════════════════════════════════════════════════════════════════════

fn kt_def(n: Node, _: &[u8], in_type: bool) -> Option<Def> {
    match n.kind() {
        "function_declaration" => Some(Def::Function),
        "class_declaration" | "object_declaration" => Some(Def::Class),
        "type_alias" => Some(Def::Type),
        "property_declaration" if !in_type => Some(Def::Const),
        _ => None,
    }
}
fn kt_name(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "type_alias" => field_text(n, "type", src).or_else(|| child_of_kind(n, "identifier").map(|c| text(c, src))),
        "property_declaration" => child_of_kind(n, "variable_declaration").and_then(|v| child_of_kind(v, "identifier")).map(|c| text(c, src)),
        _ => field_text(n, "name", src),
    }
}
fn kt_body(n: Node) -> Option<Node> {
    child_of_kinds(n, &["function_body", "class_body", "enum_class_body"])
}
fn kt_imports(n: Node, src: &[u8]) -> Vec<String> {
    if n.kind() != "import" {
        return vec![];
    }
    let mut spec = child_of_kind(n, "qualified_identifier").map(|q| text(q, src)).unwrap_or_default();
    if spec.is_empty() {
        return vec![];
    }
    if has_token(n, src, "*") || text(n, src).trim_end().ends_with(".*") {
        spec.push_str(".*");
    }
    vec![spec]
}
fn kt_callee(n: Node, src: &[u8]) -> Option<String> {
    if n.kind() != "call_expression" {
        return None;
    }
    let f = named_children(n).into_iter().next()?;
    last_ident(f, src, &["identifier"])
}
fn kt_exported(n: Node, src: &[u8], _: &str) -> bool {
    let hidden = |w: &str| matches!(w, "private" | "protected" | "internal");
    !named_children(n).iter().any(|c| c.kind() == "modifiers" && text(*c, src).split_whitespace().any(hidden))
}
fn kt_decision(n: Node, _: &[u8]) -> bool {
    match n.kind() {
        "if_expression" | "for_statement" | "while_statement" | "do_while_statement" | "catch_block" | "elvis_expression" => true,
        "when_entry" => field(n, "condition").is_some(),
        _ => false,
    }
}
pub(crate) static KOTLIN: Profile = Profile {
    def_kind: kt_def,
    name_of: kt_name,
    signature: |n, src| child_of_kind(n, "function_value_parameters").map(|p| text(p, src)).unwrap_or_else(|| "()".to_string()),
    body_of: kt_body,
    is_container: |n, _| same_if(n, &["companion_object", "class_body"]),
    imports: kt_imports,
    callee: kt_callee,
    exported: kt_exported,
    type_visibility: no_marker,
    visibility_marker: no_marker,
    is_decision: kt_decision,
    bool_op_hosts: &["binary_expression", "conjunction_expression", "disjunction_expression"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// SWIFT
// ═════════════════════════════════════════════════════════════════════════════════════════════

fn sw_def(n: Node, src: &[u8], in_type: bool) -> Option<Def> {
    match n.kind() {
        "function_declaration" | "init_declaration" | "deinit_declaration" | "protocol_function_declaration" => Some(Def::Function),
        // `class_declaration` covers class/struct/enum/actor AND `extension Foo` — an extension
        // is not a new symbol (its methods attach to the existing one), so it is a container.
        "class_declaration" => if first_token(n, src) == "extension" { None } else { Some(Def::Class) },
        "protocol_declaration" => Some(Def::Class),
        "typealias_declaration" => Some(Def::Type),
        "property_declaration" if !in_type => Some(Def::Const),
        _ => None,
    }
}
fn sw_name(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "init_declaration" => Some("init".to_string()),
        "deinit_declaration" => Some("deinit".to_string()),
        "property_declaration" => field(n, "name").and_then(|p| field_text(p, "bound_identifier", src)),
        // the FIRST `name` field is the identifier; later ones are the return type
        _ => field(n, "name").map(|x| last_ident(x, src, &["simple_identifier", "type_identifier"]).unwrap_or_else(|| text(x, src))),
    }
}
fn sw_signature(n: Node, src: &[u8]) -> String {
    let ps: Vec<String> = named_children(n).into_iter().filter(|c| c.kind() == "parameter").map(|c| text(c, src)).collect();
    format!("({})", ps.join(", "))
}
fn sw_imports(n: Node, src: &[u8]) -> Vec<String> {
    if n.kind() != "import_declaration" {
        return vec![];
    }
    child_of_kind(n, "identifier").map(|i| text(i, src)).into_iter().collect()
}
fn sw_callee(n: Node, src: &[u8]) -> Option<String> {
    if n.kind() != "call_expression" {
        return None;
    }
    let f = named_children(n).into_iter().next()?;
    last_ident(f, src, &["simple_identifier", "type_identifier"])
}
fn sw_exported(n: Node, src: &[u8], _: &str) -> bool {
    !named_children(n).iter().any(|c| c.kind() == "modifiers" && text(*c, src).split_whitespace().any(|w| matches!(w, "private" | "fileprivate")))
}
fn sw_decision(n: Node, _: &[u8]) -> bool {
    match n.kind() {
        "if_statement" | "guard_statement" | "for_statement" | "while_statement" | "repeat_while_statement" | "catch_block" | "ternary_expression" => true,
        "switch_entry" => child_of_kind(n, "default_keyword").is_none(),
        // Swift spells `&&`/`||` as their own node kinds
        "conjunction_expression" | "disjunction_expression" => true,
        _ => false,
    }
}
pub(crate) static SWIFT: Profile = Profile {
    def_kind: sw_def,
    name_of: sw_name,
    signature: sw_signature,
    body_of: body_field,
    is_container: |n, src| if n.kind() == "class_declaration" && first_token(n, src) == "extension" { Descend::AsType } else { Descend::No },
    imports: sw_imports,
    callee: sw_callee,
    exported: sw_exported,
    type_visibility: no_marker,
    visibility_marker: no_marker,
    is_decision: sw_decision,
    bool_op_hosts: &[],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// SCALA
// ═════════════════════════════════════════════════════════════════════════════════════════════

fn sc_def(n: Node, _: &[u8], in_type: bool) -> Option<Def> {
    match n.kind() {
        "function_definition" | "function_declaration" => Some(Def::Function),
        "class_definition" | "object_definition" | "trait_definition" | "enum_definition" => Some(Def::Class),
        "type_definition" => Some(Def::Type),
        "val_definition" | "var_definition" if !in_type => Some(Def::Const),
        _ => None,
    }
}
fn sc_name(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "val_definition" | "var_definition" => field(n, "pattern").filter(|p| p.kind() == "identifier").map(|p| text(p, src)),
        _ => field_text(n, "name", src),
    }
}
fn sc_imports(n: Node, src: &[u8]) -> Vec<String> {
    if n.kind() != "import_declaration" {
        return vec![];
    }
    // `import a.b.c` / `import a.b.{c, d => e}` — read off the text: the grammar spells the path
    // as N `path` fields with the dots between them anonymous.
    let t = text(n, src);
    let spec: String = t.trim().trim_start_matches("import").split_whitespace().collect();
    if spec.is_empty() {
        return vec![];
    }
    match spec.split_once('{') {
        Some((base, rest)) => {
            let base = base.trim_end_matches('.');
            rest.trim_end_matches('}')
                .split(',')
                .map(|item| item.split("=>").next().unwrap_or("").trim())
                .filter(|item| !item.is_empty())
                .map(|item| format!("{base}.{item}"))
                .collect()
        }
        None => vec![spec],
    }
}
fn sc_callee(n: Node, src: &[u8]) -> Option<String> {
    if n.kind() != "call_expression" {
        return None;
    }
    let f = field(n, "function")?;
    match f.kind() {
        "identifier" => Some(text(f, src)),
        "field_expression" => field_text(f, "field", src),
        "generic_function" => last_ident(f, src, &["identifier"]),
        _ => None,
    }
}
fn sc_exported(n: Node, src: &[u8], _: &str) -> bool {
    !named_children(n).iter().any(|c| c.kind() == "modifiers" && text(*c, src).split_whitespace().any(|w| matches!(w, "private" | "protected")))
}
fn sc_decision(n: Node, _: &[u8]) -> bool {
    matches!(n.kind(), "if_expression" | "for_expression" | "while_expression" | "do_while_expression" | "case_clause" | "catch_clause")
}
pub(crate) static SCALA: Profile = Profile {
    def_kind: sc_def,
    name_of: sc_name,
    signature: params_field,
    body_of: body_field,
    is_container: |n, _| same_if(n, &["package_object", "template_body"]),
    imports: sc_imports,
    callee: sc_callee,
    exported: sc_exported,
    type_visibility: no_marker,
    visibility_marker: no_marker,
    is_decision: sc_decision,
    bool_op_hosts: &["infix_expression"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// LUA
// ═════════════════════════════════════════════════════════════════════════════════════════════

fn lua_fn_value(n: Node) -> Option<Node> {
    // `local mul = function(a, b) ... end` — the assignment carries a function_definition value
    let list = child_of_kind(n, "expression_list")?;
    named_children(list).into_iter().find(|v| v.kind() == "function_definition")
}
fn lua_def(n: Node, _: &[u8], _: bool) -> Option<Def> {
    match n.kind() {
        // `function Widget:render()` — the colon form is a method by definition
        "function_declaration" if field(n, "name").map_or(false, |x| x.kind() == "method_index_expression") => Some(Def::Method),
        "function_declaration" => Some(Def::Function),
        "assignment_statement" if lua_fn_value(n).is_some() => Some(Def::Function),
        _ => None,
    }
}
fn lua_name(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "function_declaration" => field_text(n, "name", src),
        "assignment_statement" => child_of_kind(n, "variable_list").and_then(|v| field_text(v, "name", src)),
        _ => None,
    }
}
fn lua_signature(n: Node, src: &[u8]) -> String {
    let target = if n.kind() == "assignment_statement" { lua_fn_value(n) } else { Some(n) };
    target.and_then(|t| field_text(t, "parameters", src)).unwrap_or_else(|| "()".to_string())
}
fn lua_body(n: Node) -> Option<Node> {
    let target = if n.kind() == "assignment_statement" { lua_fn_value(n) } else { Some(n) };
    target.and_then(|t| field(t, "body"))
}
fn lua_require(call: Node, src: &[u8]) -> Option<String> {
    if call.kind() != "function_call" || field_text(call, "name", src).as_deref() != Some("require") {
        return None;
    }
    let mut strings = Vec::new();
    if let Some(a) = field(call, "arguments") {
        descendants(a, "string", &mut strings);
    }
    strings.first().map(|s| strip_quotes(&text(*s, src)))
}
fn lua_imports(n: Node, src: &[u8]) -> Vec<String> {
    match n.kind() {
        "function_call" => lua_require(n, src).into_iter().collect(),
        // `local json = require("json")`
        "assignment_statement" | "variable_declaration" => {
            let mut calls = Vec::new();
            descendants(n, "function_call", &mut calls);
            calls.iter().filter_map(|c| lua_require(*c, src)).collect()
        }
        _ => vec![],
    }
}
fn lua_callee(n: Node, src: &[u8]) -> Option<String> {
    if n.kind() != "function_call" {
        return None;
    }
    let f = field(n, "name")?;
    match f.kind() {
        "identifier" => Some(text(f, src)),
        "dot_index_expression" => field_text(f, "field", src),
        "method_index_expression" => field_text(f, "method", src),
        _ => None,
    }
}
fn lua_exported(n: Node, src: &[u8], _: &str) -> bool {
    // `local function f` / `local f = function` are file-private; a global assignment is not.
    let local_here = first_token(n, src) == "local";
    let local_parent = n.parent().map_or(false, |p| p.kind() == "variable_declaration");
    !(local_here || local_parent)
}
fn lua_decision(n: Node, _: &[u8]) -> bool {
    matches!(n.kind(), "if_statement" | "elseif_statement" | "for_statement" | "while_statement" | "repeat_statement")
}
pub(crate) static LUA: Profile = Profile {
    def_kind: lua_def,
    name_of: lua_name,
    signature: lua_signature,
    body_of: lua_body,
    is_container: |n, _| same_if(n, &["variable_declaration"]),
    imports: lua_imports,
    callee: lua_callee,
    exported: lua_exported,
    type_visibility: no_marker,
    visibility_marker: no_marker,
    is_decision: lua_decision,
    bool_op_hosts: &["binary_expression"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// ELIXIR — everything is a `call`; the target identifier says what kind of thing it is
// ═════════════════════════════════════════════════════════════════════════════════════════════

const EX_DEF_FN: &[&str] = &["def", "defp", "defmacro", "defmacrop", "defguard", "defguardp", "defdelegate"];
const EX_DEF_MOD: &[&str] = &["defmodule", "defprotocol", "defimpl"];
const EX_IMPORT: &[&str] = &["import", "alias", "require", "use"];
/// Module attributes that are documentation or compiler directives, not values.
const EX_ATTR_NOISE: &[&str] = &["moduledoc", "doc", "typedoc", "spec", "type", "typep", "opaque", "callback", "macrocallback", "behaviour", "impl", "derive", "before_compile", "after_compile", "on_definition", "external_resource", "dialyzer", "deprecated", "enforce_keys"];
/// Kernel forms that shape control flow or declarations — not edges in a call graph.
const EX_NOT_CALLS: &[&str] = &[
    "if", "unless", "case", "cond", "with", "for", "receive", "try", "quote", "unquote", "unquote_splicing", "fn", "defstruct", "defexception", "defoverridable", "@", "raise", "reraise", "throw", "when",
];

fn ex_target(n: Node, src: &[u8]) -> Option<String> {
    if n.kind() != "call" {
        return None;
    }
    let t = field(n, "target")?;
    match t.kind() {
        "identifier" => Some(text(t, src)),
        "dot" => field(t, "right").map(|r| text(r, src)),
        _ => None,
    }
}
fn ex_args(n: Node) -> Option<Node> {
    child_of_kind(n, "arguments")
}
fn ex_first_arg(n: Node) -> Option<Node> {
    ex_args(n).and_then(|a| named_children(a).into_iter().next())
}
fn ex_def(n: Node, src: &[u8], _: bool) -> Option<Def> {
    match n.kind() {
        "call" => {
            let t = ex_target(n, src)?;
            if EX_DEF_FN.contains(&t.as_str()) { Some(Def::Function) } else if EX_DEF_MOD.contains(&t.as_str()) { Some(Def::Class) } else { None }
        }
        // `@cap 10` — a module attribute is the closest thing Elixir has to a const
        "unary_operator" if first_token(n, src) == "@" => {
            let operand = field(n, "operand").or_else(|| named_children(n).into_iter().next())?;
            let attr = ex_target(operand, src)?;
            if ex_args(operand).is_some() && !EX_ATTR_NOISE.contains(&attr.as_str()) { Some(Def::Const) } else { None }
        }
        _ => None,
    }
}
fn ex_name(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "unary_operator" => field(n, "operand").or_else(|| named_children(n).into_iter().next()).and_then(|o| ex_target(o, src)),
        "call" => {
            let mut head = ex_first_arg(n)?;
            // `def add(a, b) when a > 0` — the guard wraps the head
            if head.kind() == "binary_operator" {
                head = field(head, "left")?;
            }
            match head.kind() {
                "call" => ex_target(head, src),
                "identifier" | "alias" => Some(text(head, src)),
                _ => None,
            }
        }
        _ => None,
    }
}
fn ex_signature(n: Node, src: &[u8]) -> String {
    let Some(mut head) = ex_first_arg(n) else { return "()".to_string() };
    if head.kind() == "binary_operator" {
        if let Some(l) = field(head, "left") {
            head = l;
        }
    }
    match ex_args(head).map(|a| text(a, src)) {
        Some(t) if t.starts_with('(') => t,
        Some(t) => format!("({t})"),
        None => "()".to_string(),
    }
}
fn ex_body(n: Node) -> Option<Node> {
    // `do ... end` block, or the keyword form `def f, do: expr`
    child_of_kind(n, "do_block").or_else(|| child_of_kind(ex_args(n)?, "keywords"))
}
fn ex_imports(n: Node, src: &[u8]) -> Vec<String> {
    let Some(t) = ex_target(n, src) else { return vec![] };
    if !EX_IMPORT.contains(&t.as_str()) {
        return vec![];
    }
    let Some(arg) = ex_first_arg(n) else { return vec![] };
    match arg.kind() {
        "alias" => vec![text(arg, src)],
        // `alias Acme.{Foo, Bar}` → Acme.Foo, Acme.Bar
        "dot" => {
            let left = field(arg, "left").map(|l| text(l, src)).unwrap_or_default();
            let mut aliases = Vec::new();
            if let Some(r) = field(arg, "right") {
                descendants(r, "alias", &mut aliases);
            }
            aliases.iter().map(|a| format!("{left}.{}", text(*a, src))).collect()
        }
        _ => vec![],
    }
}
fn ex_callee(n: Node, src: &[u8]) -> Option<String> {
    let t = ex_target(n, src)?;
    if EX_DEF_FN.contains(&t.as_str()) || EX_DEF_MOD.contains(&t.as_str()) || EX_IMPORT.contains(&t.as_str()) || EX_NOT_CALLS.contains(&t.as_str()) {
        return None;
    }
    Some(t)
}
fn ex_exported(n: Node, src: &[u8], _: &str) -> bool {
    !matches!(ex_target(n, src).as_deref(), Some("defp") | Some("defmacrop") | Some("defguardp"))
}
fn ex_decision(n: Node, src: &[u8]) -> bool {
    match n.kind() {
        // one per arm of case/cond/receive/fn — the wrapping call is not itself a branch
        "stab_clause" | "rescue_block" | "catch_block" => true,
        "call" => matches!(ex_target(n, src).as_deref(), Some("if") | Some("unless") | Some("with")),
        _ => false,
    }
}
pub(crate) static ELIXIR: Profile = Profile {
    def_kind: ex_def,
    name_of: ex_name,
    signature: ex_signature,
    body_of: ex_body,
    is_container: not_container,
    imports: ex_imports,
    callee: ex_callee,
    exported: ex_exported,
    type_visibility: no_marker,
    visibility_marker: no_marker,
    is_decision: ex_decision,
    bool_op_hosts: &["binary_operator"],
    methods_in_types: false,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════
// BASH
// ═════════════════════════════════════════════════════════════════════════════════════════════

/// Builtins that are statements rather than edges — recording `echo` as a call of every
/// function would drown the graph.
const SH_NOISE: &[&str] = &["echo", "printf", "set", "cd", "exit", "return", "shift", "export", "eval", "test", "[", "[[", "true", "false", ":", "local", "read", "unset", "trap", "wait", "sleep"];

fn sh_command_name(n: Node, src: &[u8]) -> Option<String> {
    if n.kind() != "command" {
        return None;
    }
    field(n, "name").map(|c| text(c, src))
}
fn sh_imports(n: Node, src: &[u8]) -> Vec<String> {
    match sh_command_name(n, src).as_deref() {
        Some("source") | Some(".") => {
            let mut c = n.walk();
            let first = n.children_by_field_name("argument", &mut c).next();
            first.map(|a| strip_quotes(&text(a, src))).into_iter().collect()
        }
        _ => vec![],
    }
}
fn sh_callee(n: Node, src: &[u8]) -> Option<String> {
    let name = sh_command_name(n, src)?;
    if SH_NOISE.contains(&name.as_str()) { None } else { Some(name) }
}
fn sh_decision(n: Node, _: &[u8]) -> bool {
    matches!(n.kind(), "if_statement" | "elif_clause" | "for_statement" | "c_style_for_statement" | "while_statement" | "until_statement" | "case_item" | "ternary_expression")
}
pub(crate) static BASH: Profile = Profile {
    def_kind: |n, _, _| if n.kind() == "function_definition" { Some(Def::Function) } else { None },
    name_of: |n, src| field_text(n, "name", src),
    signature: |_, _| "()".to_string(),
    body_of: body_field,
    is_container: not_container,
    imports: sh_imports,
    callee: sh_callee,
    exported: always_exported,
    type_visibility: no_marker,
    visibility_marker: no_marker,
    is_decision: sh_decision,
    bool_op_hosts: &["list", "binary_expression"],
    methods_in_types: true,
};

// ═════════════════════════════════════════════════════════════════════════════════════════════

/// The profile for a grammar name, or None for the grammars parse-treesitter.rs walks itself
/// (tsx / typescript / python / rust) and for anything unknown.
pub(crate) fn profile_for(grammar: &str) -> Option<&'static Profile> {
    Some(match grammar {
        "go" => &GO,
        "java" => &JAVA,
        "c_sharp" => &C_SHARP,
        "c" | "cpp" => &C,
        "php" => &PHP,
        "ruby" => &RUBY,
        "kotlin" => &KOTLIN,
        "swift" => &SWIFT,
        "scala" => &SCALA,
        "lua" => &LUA,
        "elixir" => &ELIXIR,
        "bash" => &BASH,
        _ => return None,
    })
}

/// Every grammar this module walks — the single list `ast-parse.rs` derives its analysable
/// extensions from, so adding a profile here is what makes a language visible to analysis.
pub(crate) const PROFILED_GRAMMARS: &[&str] = &["go", "java", "c_sharp", "c", "cpp", "php", "ruby", "kotlin", "swift", "scala", "lua", "elixir", "bash"];

// The thorough per-language tests (visibility, complexity, imports, class bodies) live in
// parse-treesitter.rs `profile_tests`, next to the ts/py/rust ones, so one `cargo test
// parse_treesitter` exercises every language through the same public entry point. The tests
// here are the SMALL ones: a few lines of each language, one function calling one thing.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codebase_parse_treesitter::parseTreeSitter;
    use serde_json::json;

    #[test]
    fn every_profiled_grammar_has_a_profile_and_vice_versa() {
        for g in PROFILED_GRAMMARS {
            assert!(profile_for(g).is_some(), "{g} listed but has no profile");
        }
        for g in ["tsx", "typescript", "python", "rust", "nonsense"] {
            assert!(profile_for(g).is_none(), "{g} must not be claimed here");
        }
    }

    /// Parse `src` as `grammar` and check: no parse error, a def named `def`, and a call edge
    /// from that def to `callee`. The smallest claim that "this language works".
    fn smoke(grammar: &str, path: &str, src: &str, def: &str, callee: &str) {
        let r = parseTreeSitter(&json!(src), &json!(path), &json!(grammar));
        assert!(!r.is_null(), "{grammar}: no walk profile");
        assert_eq!(r["has_error"], json!(false), "{grammar}: parse error in the snippet");
        let nodes = r["nodes"].as_array().unwrap();
        assert!(nodes.iter().any(|n| n["name"] == json!(def)), "{grammar}: no def `{def}` in {:?}", nodes.iter().map(|n| n["name"].clone()).collect::<Vec<_>>());
        let calls = r["calls"].as_array().unwrap();
        assert!(
            calls.iter().any(|c| c["caller"] == json!(def) && c["callee"] == json!(callee)),
            "{grammar}: no call {def} -> {callee} in {:?}",
            calls.iter().map(|c| format!("{}->{}", c["caller"], c["callee"])).collect::<Vec<_>>()
        );
    }

    #[test]
    fn smoke_go() {
        smoke("go", "a.go", "package main\n\nfunc Run() { helper() }\n", "Run", "helper");
    }

    #[test]
    fn smoke_java() {
        smoke("java", "A.java", "class A {\n  void run() { helper(); }\n}\n", "run", "helper");
    }

    #[test]
    fn smoke_c_sharp() {
        smoke("c_sharp", "A.cs", "class A {\n  void Run() { Helper(); }\n}\n", "Run", "Helper");
    }

    #[test]
    fn smoke_c() {
        smoke("c", "a.c", "void run(void) { helper(); }\n", "run", "helper");
    }

    #[test]
    fn smoke_cpp() {
        smoke("cpp", "a.cpp", "class A {\npublic:\n  void run() { helper(); }\n};\n", "run", "helper");
    }

    #[test]
    fn smoke_php() {
        smoke("php", "a.php", "<?php\nfunction run() { helper(); }\n", "run", "helper");
    }

    #[test]
    fn smoke_ruby() {
        // `helper()` with parens: a bare `helper` parses as an identifier, which tree-sitter
        // cannot tell apart from a local-variable read, so it is deliberately NOT a call edge.
        smoke("ruby", "a.rb", "def run\n  helper()\nend\n", "run", "helper");
    }

    #[test]
    fn smoke_kotlin() {
        smoke("kotlin", "a.kt", "fun run() {\n    helper()\n}\n", "run", "helper");
    }

    #[test]
    fn smoke_swift() {
        smoke("swift", "a.swift", "func run() {\n    helper()\n}\n", "run", "helper");
    }

    #[test]
    fn smoke_scala() {
        smoke("scala", "a.scala", "object A {\n  def run(): Unit = helper()\n}\n", "run", "helper");
    }

    #[test]
    fn smoke_lua() {
        smoke("lua", "a.lua", "function run()\n  helper()\nend\n", "run", "helper");
    }

    #[test]
    fn smoke_elixir() {
        smoke("elixir", "a.ex", "defmodule A do\n  def run do\n    helper()\n  end\nend\n", "run", "helper");
    }

    #[test]
    fn smoke_bash() {
        smoke("bash", "a.sh", "run() {\n  helper\n}\n", "run", "helper");
    }

    #[test]
    fn smoke_every_profiled_grammar_is_covered_above() {
        // If a grammar is added to PROFILED_GRAMMARS without a smoke test, this is the reminder.
        assert_eq!(PROFILED_GRAMMARS.len(), 13, "add a smoke_<lang> test for the new grammar");
    }
}
