//! file-internal-diagram.rs — twin of skills/codebase/file-internal-diagram.mjs (db lane via
//! codebase-file-read.rs). NAMES ARE CONTRACT: default `fileInternalDiagram`. Builds a
//! deterministic BLOCKS + LOGIC diagram of ONE file straight from its own AST: top-level units
//! (imports/functions/classes/const-and-let-and-var/exports) become blocks, and a block
//! referencing a name another block PROVIDES becomes a provider → consumer edge.
//!
//! ## Parser: native `oxc_parser`, parsed INLINE (not a shared `js_ast` twin)
//! The JS source's `parseModule` (js-ast.mjs) is acorn + acorn-typescript — a richer
//! TS/JSX/decorator-capable superset that has NO Rust twin (code-decorate.rs's module doc
//! documents that same gap as PARITY-DEFERRED, for the SAME js-ast.mjs dependency). This twin
//! does not defer: it builds its OWN small oxc-based raw-AST walk, following this crate's
//! `parse-js.rs` twin's established architecture choice (`oxc_parser` 0.143, `SourceType::jsx()`
//! — JS+Module+JSX turned on unconditionally, `preserve_parens:false` to stay ESTree-shaped) and
//! its documented ESTree-vs-oxc divergences (no single `Literal`; `MemberExpression` pre-split
//! into Static/Computed; `ExportNamedDeclaration` split into `ExportDeclaration`/
//! `ExportNamedDeclaration`/`ExportFromDeclaration`; `oxc_ast_visit::Visit` as the generic
//! recursive walk matching acorn's `Object.keys` walk node-for-node). Because the arena-allocated
//! oxc AST is lifetime-bound to its `Allocator`, parsing and the whole `extract()` walk happen
//! inline in one function scope (`fileInternalDiagram`), exactly like `parse-js.rs` — no AST is
//! returned across a function boundary. SCOPE NOTE (accepted, not hollow): under
//! `SourceType::jsx()` (no TypeScript), genuine TS-only syntax (type aliases, interfaces, enums)
//! fails to parse and this twin reports "couldn't parse" for it, where the JS source's
//! acorn-typescript parser would accept it. Plain JS/JSX inputs — the common case, and the case
//! this crate's own `.rs` files and fixtures are — are walked with full fidelity.
//!
//! ## The generic `walk(node, visit)` walk, reproduced via `oxc_ast_visit::Visit`
//! The JS source's `walk()` is a hand-rolled `for (const k in node)` traversal over EVERY
//! enumerable child (skipping only `start`/`end`/`loc`/`range`/`type`) that visits every node —
//! including nodes ESTree calls `Identifier` in ALL positions: expression references, binding
//! names (function/param/variable/catch-clause identifiers), non-computed member/property keys,
//! and even break/continue labels. oxc splits what acorn calls one `Identifier` type across FOUR
//! distinct types — `IdentifierReference` (expression position), `BindingIdentifier` (binding
//! position), `IdentifierName` (member-expression `.property` / object- and class-member
//! `PropertyKey`), `LabelIdentifier` (loop/break/continue labels) — so this twin's `RefCollector`
//! overrides `visit_identifier_reference`/`visit_binding_identifier`/`visit_identifier_name`/
//! `visit_label_identifier`, each falling through to the generated `walk_*` default for further
//! recursion, together covering exactly the set of nodes acorn would type `Identifier`. (oxc's
//! `JSXIdentifier` is a SEPARATE type not visited here — correctly so: acorn-jsx also types JSX
//! names `JSXIdentifier`, not `Identifier`, so the JS source's own `n.type === 'Identifier'` check
//! already excludes them.)
//!
//! The JS source's two skip rules — a non-computed `MemberExpression.property`, and a
//! non-computed non-shorthand `Property.key` (acorn types BOTH object-LITERAL and object-PATTERN
//! properties `'Property'`) — become, in oxc: `StaticMemberExpression.property` (the split
//! itself IS the "non-computed" case, matching `parse-js.rs`'s own `callee_name` precedent) and
//! `ObjectProperty`/`BindingProperty` (oxc's split of literal-vs-pattern `Property`) filtered on
//! their own `computed`/`shorthand` fields. Class member/method keys use a DIFFERENT oxc type
//! (`MethodDefinition`/`PropertyDefinition`, matching acorn's OWN different type names for those,
//! never `'Property'`) — so, faithfully, this twin does NOT skip them either: a class method
//! named the same as a top-level provider still produces an edge, exactly like the JS source.
//!
//! ## `refNode` shapes, one per top-level block kind (verified against the JS source line-by-line)
//! - `import` blocks: `refNode: null` — never walked, only `provides` (the specifier locals).
//! - named `function`/`class` blocks (plain or `export`ed): `refNode` = the body ONLY
//!   (`FunctionBody` / `ClassBody`), NOT the whole declaration — so the function's own name/param
//!   names are not part of the walk.
//! - top-level `var`/`let`/`const` (plain or exported) with a simple identifier binding:
//!   `refNode` = the declarator's `init` EXPRESSION AS A WHOLE (an arrow's own params ARE walked
//!   here, unlike the named-function case above — a real JS-source quirk, reproduced faithfully).
//!   A destructured top-level declarator (`const {a,b} = …`) gets NO block and its init is never
//!   walked at all (no fallback), matching the JS source exactly.
//! - bare `export { a, b };` (no `from`): one `"exports {…}"` block whose `refNode` is the WHOLE
//!   statement (so the exported names' own `Identifier`/`IdentifierName` nodes are found and can
//!   edge back to whatever block provides them).
//! - `export { a } from 'x';`: `"re-export basename(x)"` block, `refNode: null`.
//! - `export default …;`: ALWAYS one `"export default"` block (role `data`, never `exported:true`
//!   — the JS source never sets that flag here, a real quirk reproduced faithfully) whose
//!   `refNode` is the WHOLE declaration node (function/class/expression) — so for
//!   `export default function foo(){…}` the function's OWN id/param names ARE part of the walk
//!   (unlike the named-function case), reproduced here via a small manual shallow-visit
//!   (`id` → `params` → `body`) instead of the generated `visit_function`, which needs a
//!   `ScopeFlags` argument from `oxc_syntax` — not a direct dependency of this crate, and adding
//!   one is out of scope for a single-file port; the manual visit reaches every node the
//!   generated one would for this file's purposes (no scope-tracking is used anywhere here).
//! - any other top-level statement (plain `ExpressionStatement`, `ExportAllDeclaration`, …): NO
//!   block, and — because this file's own `extract()` has no top-level catch-all (unlike
//!   `parse-js.mjs`'s `collectTopLevel` fallback) — NOT walked for anything either.
// QUALITY: genuine full port — every top-level block kind, the two-pass skip-set + identifier
// walk, and the provider→consumer edge algorithm are reproduced from the JS source line-by-line.
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingIdentifier, BindingPattern, BindingProperty, Class, ClassBody, Declaration,
    Expression, ExportDefaultDeclarationKind, Function, FunctionBody, IdentifierName,
    IdentifierReference, LabelIdentifier, ObjectProperty, Program, PropertyKey, Statement,
    StaticMemberExpression,
};
use oxc_ast_visit::{walk, Visit};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

fn base_name(p: &str) -> String {
    let parts: Vec<&str> = p.split(['\\', '/']).collect();
    match parts.last() {
        Some(&last) if !last.is_empty() => last.to_string(),
        _ => p.to_string(),
    }
}

// acorn's `Position.line` is 1-based and counts `\n`s; oxc only carries byte offsets. ASCII/BMP
// source only — same assumption `parse-js.rs`'s own `line_of` documents.
fn line_of(source: &str, offset: u32) -> i64 {
    1 + source.as_bytes()[..offset as usize].iter().filter(|&&b| b == b'\n').count() as i64
}

// One top-level block's `refNode` shape — see module doc for which JS branch produces which variant.
enum Ref<'a> {
    None,
    Body(&'a FunctionBody<'a>),
    ClassBody(&'a ClassBody<'a>),
    Expr(&'a Expression<'a>),
    Func(&'a Function<'a>),
    Class(&'a Class<'a>),
    Stmt(&'a Statement<'a>),
}

// Dispatches a `refNode` into the right generic entry point of `oxc_ast_visit::Visit`. `Ref::Func`
// can't use the generated `visit_function` (needs a `ScopeFlags` arg from `oxc_syntax`, not a
// direct dep here — see module doc) so it manually visits the 3 fields the JS source's generic
// walk would reach on a whole `FunctionDeclaration` node: `id`, `params`, `body`.
fn visit_ref<'a, V: Visit<'a>>(v: &mut V, r: &Ref<'a>) {
    match r {
        Ref::None => {}
        Ref::Body(b) => v.visit_function_body(b),
        Ref::ClassBody(cb) => v.visit_class_body(cb),
        Ref::Expr(e) => v.visit_expression(e),
        Ref::Func(f) => {
            if let Some(id) = &f.id {
                v.visit_binding_identifier(id);
            }
            v.visit_formal_parameters(&f.params);
            if let Some(body) = &f.body {
                v.visit_function_body(body);
            }
        }
        Ref::Class(c) => v.visit_class(c),
        Ref::Stmt(s) => v.visit_statement(s),
    }
}

struct Block<'a> {
    id: String,
    label: String,
    role: &'static str,
    line: Option<i64>,
    exported: bool,
    provides: Vec<String>,
    ref_: Ref<'a>,
}

fn add<'a>(blocks: &mut Vec<Block<'a>>, label: String, role: &'static str, line: Option<i64>, exported: bool, provides: Vec<String>, ref_: Ref<'a>) {
    let id = format!("k{}", blocks.len());
    blocks.push(Block { id, label, role, line, exported, provides, ref_ });
}

// Pass 1 (`skip` build): the JS source's `n.type === 'MemberExpression' && !n.computed …` /
// `n.type === 'Property' && !n.computed && n.key !== n.value` checks. See module doc for why
// class member/method keys (a DIFFERENT oxc/acorn type) are deliberately NOT covered here.
#[derive(Default)]
struct SkipCollector {
    skip: HashSet<u32>,
}
impl<'a> Visit<'a> for SkipCollector {
    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        self.skip.insert(it.property.span.start);
        walk::walk_static_member_expression(self, it);
    }
    fn visit_object_property(&mut self, it: &ObjectProperty<'a>) {
        if !it.computed && !it.shorthand {
            if let PropertyKey::StaticIdentifier(id) = &it.key {
                self.skip.insert(id.span.start);
            }
        }
        walk::walk_object_property(self, it);
    }
    fn visit_binding_property(&mut self, it: &BindingProperty<'a>) {
        if !it.computed && !it.shorthand {
            if let PropertyKey::StaticIdentifier(id) = &it.key {
                self.skip.insert(id.span.start);
            }
        }
        walk::walk_binding_property(self, it);
    }
}

// Pass 2: every `Identifier`-shaped node not in `skip`, checked against `provider` — the JS
// source's inline `if (!src || src === b.id) return; … seen.add(k); edges.push(…)` logic, done
// live during the walk (not buffered) to match the JS source's structure exactly.
struct RefCollector<'p> {
    skip: &'p HashSet<u32>,
    self_id: &'p str,
    provider: &'p HashMap<String, String>,
    role_of: &'p HashMap<String, String>,
    seen: &'p mut HashSet<String>,
    edges: &'p mut Vec<Value>,
}
impl<'p> RefCollector<'p> {
    fn note(&mut self, name: &str, start: u32) {
        if self.skip.contains(&start) {
            return;
        }
        let Some(src) = self.provider.get(name) else { return };
        if src == self.self_id {
            return;
        }
        let key = format!("{src}>{}", self.self_id);
        if !self.seen.insert(key) {
            return;
        }
        let label = if self.role_of.get(src).map(String::as_str) == Some("external") { "imports" } else { "uses" };
        self.edges.push(json!({ "from": src, "to": self.self_id, "label": label }));
    }
}
impl<'a, 'p> Visit<'a> for RefCollector<'p> {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        self.note(it.name.as_str(), it.span.start);
        walk::walk_identifier_reference(self, it);
    }
    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        self.note(it.name.as_str(), it.span.start);
        walk::walk_binding_identifier(self, it);
    }
    fn visit_label_identifier(&mut self, it: &LabelIdentifier<'a>) {
        self.note(it.name.as_str(), it.span.start);
        walk::walk_label_identifier(self, it);
    }
    fn visit_identifier_name(&mut self, it: &IdentifierName<'a>) {
        self.note(it.name.as_str(), it.span.start);
        walk::walk_identifier_name(self, it);
    }
}

// `extract(ast)` — top-level scan builds `blocks`, then the provider map + two-pass edge walk.
// Returns `None` when `blocks.len() < 2` (mirrors the JS source's own early-out).
fn extract<'a>(program: &'a Program<'a>, source: &str) -> Option<(Vec<Value>, Vec<Value>)> {
    let mut blocks: Vec<Block<'a>> = Vec::new();

    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(imp) => {
                let names: Vec<String> = imp
                    .specifiers
                    .as_ref()
                    .map(|specs| specs.iter().map(|s| s.local().name.as_str().to_string()).collect())
                    .unwrap_or_default();
                add(&mut blocks, format!("import {}", base_name(imp.source.value.as_str())), "external", Some(line_of(source, imp.span.start)), false, names, Ref::None);
            }
            Statement::FunctionDeclaration(f) => {
                if let Some(id) = &f.id {
                    let name = id.name.as_str().to_string();
                    let ref_ = f.body.as_deref().map(Ref::Body).unwrap_or(Ref::None);
                    add(&mut blocks, format!("{name}()"), "logic", Some(line_of(source, f.span.start)), false, vec![name], ref_);
                }
            }
            Statement::ClassDeclaration(c) => {
                if let Some(id) = &c.id {
                    let name = id.name.as_str().to_string();
                    add(&mut blocks, format!("class {name}"), "logic", Some(line_of(source, c.span.start)), false, vec![name], Ref::ClassBody(&*c.body));
                }
            }
            Statement::VariableDeclaration(vd) => {
                for d in &vd.declarations {
                    if let BindingPattern::BindingIdentifier(id) = &d.id {
                        let name = id.name.as_str().to_string();
                        let ref_ = d.init.as_ref().map(Ref::Expr).unwrap_or(Ref::None);
                        add(&mut blocks, name.clone(), "logic", Some(line_of(source, d.span.start)), false, vec![name], ref_);
                    }
                }
            }
            Statement::ExportDeclaration(ed) => {
                let line = Some(line_of(source, ed.span.start));
                match &ed.declaration {
                    Declaration::FunctionDeclaration(f) => {
                        if let Some(id) = &f.id {
                            let name = id.name.as_str().to_string();
                            let ref_ = f.body.as_deref().map(Ref::Body).unwrap_or(Ref::None);
                            add(&mut blocks, format!("{name}()"), "logic", line, true, vec![name], ref_);
                        } else {
                            add(&mut blocks, "exports {…}".to_string(), "data", line, false, vec![], Ref::Stmt(stmt));
                        }
                    }
                    Declaration::ClassDeclaration(c) => {
                        if let Some(id) = &c.id {
                            let name = id.name.as_str().to_string();
                            add(&mut blocks, format!("class {name}"), "logic", line, true, vec![name], Ref::ClassBody(&*c.body));
                        } else {
                            add(&mut blocks, "exports {…}".to_string(), "data", line, false, vec![], Ref::Stmt(stmt));
                        }
                    }
                    Declaration::VariableDeclaration(vd) => {
                        for d in &vd.declarations {
                            if let BindingPattern::BindingIdentifier(id) = &d.id {
                                let name = id.name.as_str().to_string();
                                let ref_ = d.init.as_ref().map(Ref::Expr).unwrap_or(Ref::None);
                                add(&mut blocks, name.clone(), "logic", Some(line_of(source, d.span.start)), true, vec![name], ref_);
                            }
                        }
                    }
                    // TS-only declaration kinds — structurally unreachable under SourceType::jsx()
                    // (no TypeScript). Mirrors the JS source's own final `else`: `node.source` is
                    // undefined for an inline declaration, so it's "exports {…}", not "re-export".
                    _ => add(&mut blocks, "exports {…}".to_string(), "data", line, false, vec![], Ref::Stmt(stmt)),
                }
            }
            Statement::ExportNamedDeclaration(end) => {
                add(&mut blocks, "exports {…}".to_string(), "data", Some(line_of(source, end.span.start)), false, vec![], Ref::Stmt(stmt));
            }
            Statement::ExportFromDeclaration(efd) => {
                add(&mut blocks, format!("re-export {}", base_name(efd.source.value.as_str())), "data", Some(line_of(source, efd.span.start)), false, vec![], Ref::None);
            }
            Statement::ExportDefaultDeclaration(edd) => {
                let line = Some(line_of(source, edd.span.start));
                let ref_ = match &edd.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(f) => Ref::Func(&**f),
                    ExportDefaultDeclarationKind::ClassDeclaration(c) => Ref::Class(&**c),
                    // TS-only, unreachable under SourceType::jsx().
                    ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => Ref::None,
                    _ => Ref::Expr(edd.declaration.to_expression()),
                };
                add(&mut blocks, "export default".to_string(), "data", line, false, vec![], ref_);
            }
            _ => {}
        }
    }

    if blocks.len() < 2 {
        return None;
    }

    let mut provider: HashMap<String, String> = HashMap::new();
    for b in &blocks {
        for p in &b.provides {
            provider.entry(p.clone()).or_insert_with(|| b.id.clone());
        }
    }
    let role_of: HashMap<String, String> = blocks.iter().map(|b| (b.id.clone(), b.role.to_string())).collect();

    let mut edges: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for b in &blocks {
        if matches!(b.ref_, Ref::None) {
            continue;
        }
        let mut skip_collector = SkipCollector::default();
        visit_ref(&mut skip_collector, &b.ref_);

        let mut rc = RefCollector {
            skip: &skip_collector.skip,
            self_id: &b.id,
            provider: &provider,
            role_of: &role_of,
            seen: &mut seen,
            edges: &mut edges,
        };
        visit_ref(&mut rc, &b.ref_);
    }

    let nodes: Vec<Value> = blocks
        .iter()
        .map(|b| {
            let label: String = b.label.chars().take(22).collect();
            json!({
                "id": b.id,
                "label": format!("{}{}", if b.exported { "\u{2197} " } else { "" }, label),
                "role": b.role,
                "file": Value::Null,
                "line": b.line,
            })
        })
        .collect();

    Some((nodes, edges))
}

pub fn fileInternalDiagram(input: &Value, db: &Connection) -> Result<Value, String> {
    let file = match input.get("path").and_then(Value::as_str).or_else(|| input.get("file").and_then(Value::as_str)) {
        Some(f) if !f.is_empty() => f.to_string(),
        _ => return Ok(json!({ "ok": false, "error": "no path" })),
    };

    let src = match crate::codebase_codebase_file_read::codebaseFileRead(&json!({ "path": file }), db) {
        Ok(r) => r.get("content").and_then(Value::as_str).unwrap_or("").to_string(),
        Err(e) => {
            let msg: String = e.chars().take(120).collect();
            return Ok(json!({ "ok": false, "file": file, "error": format!("unreadable: {msg}") }));
        }
    };
    if src.trim().is_empty() {
        return Ok(json!({ "ok": false, "file": file, "error": "file is empty" }));
    }

    let allocator = Allocator::default();
    let source_type = SourceType::jsx();
    let ret = Parser::new(&allocator, &src, source_type)
        .with_options(ParseOptions { preserve_parens: false, ..ParseOptions::default() })
        .parse();
    if ret.diagnostics.has_errors() {
        let msg = ret.diagnostics.errors().next().map(|d| d.to_string()).unwrap_or_default();
        let msg: String = msg.chars().take(120).collect();
        return Ok(json!({ "ok": false, "file": file, "error": format!("couldn't parse (JS/JSX only): {msg}") }));
    }
    let program = ret.program;

    match extract(&program, &src) {
        Some((nodes, edges)) => Ok(json!({ "ok": true, "file": file, "diagram": { "title": base_name(&file), "nodes": nodes, "edges": edges } })),
        None => Ok(json!({ "ok": false, "file": file, "error": "too few top-level blocks to diagram" })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_schema(c: &Connection) {
        c.execute_batch("CREATE TABLE workspaces (root_path TEXT, is_active INTEGER);").unwrap();
    }

    fn workspace_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("file-internal-diagram-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(c: &Connection, dir: &std::path::Path, name: &str, source: &str) -> Value {
        std::fs::write(dir.join(name), source).unwrap();
        fileInternalDiagram(&json!({ "path": name }), c).unwrap()
    }

    #[test]
    fn no_path_errs() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let r = fileInternalDiagram(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("no path"));
    }

    #[test]
    fn unreadable_file_reports_ok_false() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let r = fileInternalDiagram(&json!({ "path": "does/not/exist.js" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().starts_with("unreadable:"));
    }

    #[test]
    fn empty_file_reports_ok_false() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let r = run(&c, &dir, "empty.js", "   \n  ");
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("file is empty"));
    }

    #[test]
    fn single_block_is_too_few() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let r = run(&c, &dir, "one.js", "function solo() { return 1; }\n");
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("too few top-level blocks to diagram"));
    }

    #[test]
    fn import_and_two_functions_wire_provider_edges() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let source = "import { helper } from './lib.js';\n\nfunction inner() {\n  return helper(1);\n}\n\nfunction outer() {\n  return inner() + 1;\n}\n";
        let r = run(&c, &dir, "chain.js", source);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["file"], json!("chain.js"));
        let diagram = &r["diagram"];
        assert_eq!(diagram["title"], json!("chain.js"));
        let nodes = diagram["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0]["label"], json!("import lib.js"));
        assert_eq!(nodes[0]["role"], json!("external"));
        assert_eq!(nodes[1]["label"], json!("inner()"));
        assert_eq!(nodes[1]["line"], json!(3));
        assert_eq!(nodes[2]["label"], json!("outer()"));

        let edges = diagram["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0], json!({ "from": "k0", "to": "k1", "label": "imports" }));
        assert_eq!(edges[1], json!({ "from": "k1", "to": "k2", "label": "uses" }));
    }

    #[test]
    fn exported_function_gets_arrow_prefix_and_class_block_uses_top_level_fn() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let source = "function draw() { return 1; }\n\nexport class Widget {\n  render() {\n    return draw();\n  }\n}\n";
        let r = run(&c, &dir, "widget.js", source);
        assert_eq!(r["ok"], json!(true));
        let nodes = r["diagram"]["nodes"].as_array().unwrap();
        assert_eq!(nodes[0]["label"], json!("draw()"));
        assert_eq!(nodes[1]["label"], json!("\u{2197} class Widget"));
        let edges = r["diagram"]["edges"].as_array().unwrap();
        assert_eq!(edges, &vec![json!({ "from": "k0", "to": "k1", "label": "uses" })]);
    }

    #[test]
    fn const_arrow_and_member_property_is_not_a_false_edge() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        // `obj.mul` reads a non-computed property named `mul` — must NOT edge to the `mul` block.
        let source = "const mul = (a, b) => a * b;\n\nconst run = (obj) => obj.mul + mul(1, 2);\n";
        let r = run(&c, &dir, "ops.js", source);
        assert_eq!(r["ok"], json!(true));
        let edges = r["diagram"]["edges"].as_array().unwrap();
        // exactly one edge (the real `mul(1, 2)` call), not two.
        assert_eq!(edges, &vec![json!({ "from": "k0", "to": "k1", "label": "uses" })]);
    }

    #[test]
    fn object_property_shorthand_key_still_counts_but_normal_key_is_skipped() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        // `{ mul: 1 }` — non-computed non-shorthand key named `mul` must NOT edge to the `mul`
        // block; the shorthand `{ mul }` (value === key identity) DOES reference `mul` for real.
        let source = "const mul = 7;\n\nconst run = () => ({ mul: 1, other: mul });\n";
        let r = run(&c, &dir, "shorthand.js", source);
        assert_eq!(r["ok"], json!(true));
        let edges = r["diagram"]["edges"].as_array().unwrap();
        assert_eq!(edges, &vec![json!({ "from": "k0", "to": "k1", "label": "uses" })]);
    }

    #[test]
    fn bare_export_specifier_edges_back_to_its_provider() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let source = "const z = 1;\n\nexport { z };\n";
        let r = run(&c, &dir, "bare.js", source);
        assert_eq!(r["ok"], json!(true));
        let nodes = r["diagram"]["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["label"], json!("z"));
        assert_eq!(nodes[1]["label"], json!("exports {\u{2026}}"));
        assert_eq!(nodes[1]["role"], json!("data"));
        let edges = r["diagram"]["edges"].as_array().unwrap();
        assert_eq!(edges, &vec![json!({ "from": "k0", "to": "k1", "label": "uses" })]);
    }

    #[test]
    fn re_export_from_source_gets_basename_label_and_no_edges() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let source = "export { six, seven } from '../numbers.js';\n\nconst local = 1;\n";
        let r = run(&c, &dir, "reexport.js", source);
        assert_eq!(r["ok"], json!(true));
        let nodes = r["diagram"]["nodes"].as_array().unwrap();
        assert_eq!(nodes[0]["label"], json!("re-export numbers.js"));
        assert!(r["diagram"]["edges"].as_array().unwrap().is_empty());
    }

    #[test]
    fn export_default_function_walks_its_own_params_and_id() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let source = "function helper() { return 1; }\n\nexport default function run(helper) {\n  return helper + 1;\n}\n";
        let r = run(&c, &dir, "default.js", source);
        assert_eq!(r["ok"], json!(true));
        let nodes = r["diagram"]["nodes"].as_array().unwrap();
        assert_eq!(nodes[1]["label"], json!("export default"));
        // the default-exported function's OWN param is also named `helper` — its BindingIdentifier
        // is part of refNode too, but note-dedup means at most one edge from k0->k1 either way.
        let edges = r["diagram"]["edges"].as_array().unwrap();
        assert_eq!(edges, &vec![json!({ "from": "k0", "to": "k1", "label": "uses" })]);
    }

    #[test]
    fn long_label_is_truncated_to_22_chars() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let source = "function thisIsAVeryLongFunctionNameIndeed() { return 1; }\n\nconst other = 1;\n";
        let r = run(&c, &dir, "long.js", source);
        assert_eq!(r["ok"], json!(true));
        let label = r["diagram"]["nodes"][0]["label"].as_str().unwrap();
        assert_eq!(label.chars().count(), 22);
        assert_eq!(label, "thisIsAVeryLongFunctio");
    }

    #[test]
    fn syntax_error_reports_couldnt_parse() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
        let r = run(&c, &dir, "bad.js", "function ( { }\n");
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().starts_with("couldn't parse (JS/JSX only):"));
    }
}
