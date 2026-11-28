//! code-transform.rs — twin of skills/codebase/code-transform.mjs (db lane via
//! codebase-file-read.rs). NAMES ARE CONTRACT: default `codeTransform`. Deterministic AST-guided
//! TEXT edits to a real file — no AST-to-code round-trip, formatting/comments preserved. Two ops:
//! `rename` (a local identifier's references, scope-naive) and `rewire_import` (an import/export/
//! require module specifier).
//!
//! ## Parser: native `oxc_parser`, parsed INLINE — same architecture as parse-js.rs / file-internal-diagram.rs
//! The JS source's `parseModule` (js-ast.mjs) is acorn + acorn-typescript. As with
//! `file-internal-diagram.rs` (no shared `js_ast` twin exists — see that file's own module doc for
//! why it doesn't defer), this twin builds its own small oxc-based walk: `oxc_parser` 0.143,
//! `SourceType::jsx()` (JS+Module+JSX unconditionally), `preserve_parens:false` to stay
//! ESTree-shaped. Because the arena-allocated AST is lifetime-bound to its `Allocator`, parse +
//! walk + edit happen inline in one function scope (`codeTransform`) — no AST crosses a function
//! boundary. SCOPE NOTE (accepted, not hollow): genuine TS-only syntax fails to parse under
//! `SourceType::jsx()` (no TypeScript) — same documented gap as `file-internal-diagram.rs`.
//!
//! ## acorn's single `Identifier` type -> oxc's four types (same split as file-internal-diagram.rs)
//! Acorn types EVERY identifier-shaped node (expression references, bindings, non-computed
//! member/property names, break/continue labels) `Identifier` — the JS source's generic
//! `walk(node, visit)` therefore renames all of them (scope-naive; the JS module doc says so:
//! "a shadowed binding is also renamed"). oxc splits this into `IdentifierReference` /
//! `BindingIdentifier` / `IdentifierName` / `LabelIdentifier`; `RenameCollector` below overrides
//! all four, matching `file-internal-diagram.rs`'s own `RefCollector` precedent for the identical
//! acorn-vs-oxc split.
//!
//! ## Skip rules (never renamed) — a subtly DIFFERENT predicate than file-internal-diagram.rs's
//! A non-computed `MemberExpression.property` (oxc: `StaticMemberExpression.property`), same as
//! `file-internal-diagram.rs`. The object-property-key rule looks textually similar to that file's
//! own skip rule but is NOT: the JS source's guard is `!n.computed && n.key?.type === 'Identifier'
//! && n.key !== n.value` — that last clause checks object IDENTITY, and acorn always allocates
//! `key` and `value` as two DISTINCT node objects even for a shorthand property `{ mul }` (verified
//! live: `prop.key === prop.value` is `false`), so the identity check is true for EVERY
//! non-computed property, shorthand or not — the `shorthand` flag plays no role in this guard at
//! all. But this skill's edit-collection is keyed by SPAN START, and a shorthand property's `key`
//! and `value` nodes span the exact same source range (same start offset) — so once the key is
//! added to `skip`, the value identifier that shares its start offset is *coincidentally* skipped
//! too. Net effect, verified live against the real JS: a shorthand property `{ mul }` is NEVER
//! renamed (neither as a binding-shorthand `const { mul } = x` nor an expression-shorthand
//! `{ mul }`), while an explicit `{ mul: x }` / `{ k: mul }` renames the VALUE side normally (key
//! untouched, value's own distinct start offset isn't in `skip`). So — unlike
//! `file-internal-diagram.rs`'s `SkipCollector`, which deliberately keeps `!it.shorthand` in its
//! guard — this twin's `visit_object_property`/`visit_binding_property` skip whenever
//! `!computed && key is a plain identifier`, full stop, on BOTH `ObjectProperty` (object
//! expressions) and `BindingProperty` (object patterns) — acorn types both `'Property'`.
//!
//! ## ImportSpecifier / ExportSpecifier: rename the LOCAL binding, keep the public/external name
//! `import { imported as local }` / `export { local as exported }` — the JS source renames only
//! the LOCAL side, re-emitting an explicit `as` alias when the local name changes but the
//! public/external (module-facing) name must stay put. oxc types the "other side" of each
//! (`imported`, and BOTH `local`+`exported` for export) as `ModuleExportName`, a 3-way enum
//! (`IdentifierName` | `IdentifierReference` | `StringLiteral` — the last for the string-module-
//! export-names proposal, e.g. `export { x as "y" }`). `men_name` extracts a renderable name for
//! the first two variants and returns `None` for `StringLiteral`, mirroring the JS source's own
//! `n.imported?.name ?? from` (a `Literal` node's `.name` is `undefined` in JS too, so the JS
//! source's fallback silently reuses `from` there — reproduced by `unwrap_or_else`).
//!
//! ## rewire_import: acorn's ExportNamedDeclaration is 3 oxc types (see parse-js.rs's module doc)
//! Only the two WITH a `source` (oxc's `ExportFromDeclaration` = acorn's `export {a} from 'x'`, and
//! `ExportAllDeclaration` = `export * from 'x'`) can match a rewire — oxc's bare-specifier
//! `ExportNamedDeclaration` (`export {a};`, no `from`) has no `source` field at all, so it's
//! structurally excluded, matching the JS source's `n.source?.type === 'Literal'` guard falling
//! through on `undefined` there.
//!
//! ## `no db` branch — structurally unreachable here
//! The JS source guards `if (!db) return { ok:false, error:'no db' }`; this twin's signature takes
//! `db: &Connection` (the dispatcher always supplies one), so that branch has no Rust equivalent —
//! same convention as every other db-lane twin in this crate (e.g. `file-internal-diagram.rs`).
//!
//! ## `args.from`/`args.to` falsy-check: coerced-string domain, not raw-JS-value domain
//! The JS source's `!args.from || !args.to` runs on the RAW JSON value (so a literal `0` or `false`
//! would also be treated as missing, before the later `String(args.from)` coercion). This twin
//! reads `args.from`/`args.to` as strings directly (`args.get("from").and_then(Value::as_str)`,
//! defaulting to `""`) and checks emptiness — `from`/`to` are always identifier or module-specifier
//! strings in this skill's real call sites (never numbers/booleans), so the two checks agree on
//! every input this skill is ever actually called with.
// QUALITY: genuine full port — both ops' AST walks, the skip/handled two-pass rename algorithm, and
// the descending-sort splice are reproduced from the JS source line-by-line.
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, BindingIdentifier, BindingProperty, CallExpression, ExportAllDeclaration,
    ExportFromDeclaration, ExportSpecifier, Expression, IdentifierName, IdentifierReference,
    ImportDeclaration, ImportSpecifier, LabelIdentifier, ModuleExportName, ObjectProperty,
    Program, PropertyKey, StaticMemberExpression,
};
use oxc_ast_visit::{walk, Visit};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::{SourceType, Span};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

struct Edit {
    start: u32,
    end: u32,
    text: String,
}

// apply {start,end,text} edits right-to-left so earlier offsets stay valid — exact twin of the JS
// source's `applyEdits`. oxc span offsets always land on UTF-8 char boundaries (token boundaries),
// so byte-slicing `out` at `start`/`end` never panics on multi-byte source text.
fn apply_edits(src: &str, mut edits: Vec<Edit>) -> String {
    edits.sort_by(|a, b| b.start.cmp(&a.start));
    let mut out = src.to_string();
    for e in edits {
        let (start, end) = (e.start as usize, e.end as usize);
        out = format!("{}{}{}", &out[..start], e.text, &out[end..]);
    }
    out
}

// See module doc: ModuleExportName's `StringLiteral` variant has no renderable `.name` in JS
// either (`n.imported?.name` is `undefined` for a `Literal` node) — `None` mirrors that.
fn men_name(m: &ModuleExportName) -> Option<String> {
    match m {
        ModuleExportName::IdentifierName(id) => Some(id.name.as_str().to_string()),
        ModuleExportName::IdentifierReference(id) => Some(id.name.as_str().to_string()),
        ModuleExportName::StringLiteral(_) => None,
    }
}

fn men_span(m: &ModuleExportName) -> Span {
    match m {
        ModuleExportName::IdentifierName(id) => id.span,
        ModuleExportName::IdentifierReference(id) => id.span,
        ModuleExportName::StringLiteral(sl) => sl.span,
    }
}

// Pass 1 of `renameEdits`: builds the `skip` set (member-property / non-shorthand property-key
// positions, never renamed) AND the whole-specifier edits + `handled` set for ImportSpecifier /
// ExportSpecifier, in ONE walk — exactly like the JS source's first `walk(ast, ...)` call, which
// checks all four node-type conditions inside a single visitor callback.
struct SkipSpecifierCollector<'s> {
    from: &'s str,
    to: &'s str,
    skip: HashSet<u32>,
    handled: HashSet<u32>,
    edits: Vec<Edit>,
}

impl<'s> SkipSpecifierCollector<'s> {
    fn new(from: &'s str, to: &'s str) -> Self {
        Self { from, to, skip: HashSet::new(), handled: HashSet::new(), edits: Vec::new() }
    }
}

impl<'a, 's> Visit<'a> for SkipSpecifierCollector<'s> {
    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        self.skip.insert(it.property.span.start);
        walk::walk_static_member_expression(self, it);
    }
    fn visit_object_property(&mut self, it: &ObjectProperty<'a>) {
        // NOT `!it.shorthand` — see module doc: the JS source's identity check (`key !== value`)
        // is always true in acorn, shorthand or not; the shorthand case is excluded from renaming
        // only as a coincidence of key/value sharing one span start.
        if !it.computed {
            if let PropertyKey::StaticIdentifier(id) = &it.key {
                self.skip.insert(id.span.start);
            }
        }
        walk::walk_object_property(self, it);
    }
    fn visit_binding_property(&mut self, it: &BindingProperty<'a>) {
        if !it.computed {
            if let PropertyKey::StaticIdentifier(id) = &it.key {
                self.skip.insert(id.span.start);
            }
        }
        walk::walk_binding_property(self, it);
    }
    // `import { imported as from }`: rename the local binding, KEEP the imported (external) name.
    fn visit_import_specifier(&mut self, it: &ImportSpecifier<'a>) {
        if it.local.name.as_str() == self.from {
            let imp = men_name(&it.imported).unwrap_or_else(|| self.from.to_string());
            let text = if imp == self.to { self.to.to_string() } else { format!("{imp} as {}", self.to) };
            self.edits.push(Edit { start: it.span.start, end: it.span.end, text });
            self.handled.insert(it.local.span.start);
            self.handled.insert(men_span(&it.imported).start);
        }
        walk::walk_import_specifier(self, it);
    }
    // `export { from } / export { from as pub }`: rename the LOCAL binding, KEEP the public name.
    fn visit_export_specifier(&mut self, it: &ExportSpecifier<'a>) {
        if men_name(&it.local).as_deref() == Some(self.from) {
            let publ = men_name(&it.exported).unwrap_or_else(|| self.from.to_string());
            let text = if publ == self.to { self.to.to_string() } else { format!("{} as {publ}", self.to) };
            self.edits.push(Edit { start: it.span.start, end: it.span.end, text });
            self.handled.insert(men_span(&it.local).start);
            self.handled.insert(men_span(&it.exported).start);
        }
        walk::walk_export_specifier(self, it);
    }
}

// Pass 2 of `renameEdits`: every Identifier-shaped node named `from`, not in `skip`/`handled` —
// the JS source's second `walk(ast, ...)` call, covering the SAME four oxc types RefCollector does
// in file-internal-diagram.rs (see module doc).
struct RenameCollector<'s> {
    from: &'s str,
    to: &'s str,
    skip: &'s HashSet<u32>,
    handled: &'s HashSet<u32>,
    edits: Vec<Edit>,
}

impl<'s> RenameCollector<'s> {
    fn maybe_push(&mut self, name: &str, span: Span) {
        if name == self.from && !self.skip.contains(&span.start) && !self.handled.contains(&span.start) {
            self.edits.push(Edit { start: span.start, end: span.end, text: self.to.to_string() });
        }
    }
}

impl<'a, 's> Visit<'a> for RenameCollector<'s> {
    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        self.maybe_push(it.name.as_str(), it.span);
        walk::walk_identifier_reference(self, it);
    }
    fn visit_binding_identifier(&mut self, it: &BindingIdentifier<'a>) {
        self.maybe_push(it.name.as_str(), it.span);
        walk::walk_binding_identifier(self, it);
    }
    fn visit_identifier_name(&mut self, it: &IdentifierName<'a>) {
        self.maybe_push(it.name.as_str(), it.span);
        walk::walk_identifier_name(self, it);
    }
    fn visit_label_identifier(&mut self, it: &LabelIdentifier<'a>) {
        self.maybe_push(it.name.as_str(), it.span);
        walk::walk_label_identifier(self, it);
    }
}

fn rename_edits(program: &Program, from: &str, to: &str) -> Vec<Edit> {
    let mut sc = SkipSpecifierCollector::new(from, to);
    sc.visit_program(program);
    let mut rc = RenameCollector { from, to, skip: &sc.skip, handled: &sc.handled, edits: Vec::new() };
    rc.visit_program(program);
    let mut edits = sc.edits;
    edits.extend(rc.edits);
    edits
}

// `importEdits`: rewrites a module specifier's STRING CONTENT (inside the quotes) for
// import/export-from/export-all declarations and `require('...')` calls. `+1`/`-1` strip the
// literal's own quote characters — exact twin of the JS source's `n.source.start + 1, n.source.end - 1`.
struct ImportEditCollector<'s> {
    from: &'s str,
    to: &'s str,
    edits: Vec<Edit>,
}

impl<'a, 's> Visit<'a> for ImportEditCollector<'s> {
    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        if it.source.value.as_str() == self.from {
            self.edits.push(Edit { start: it.source.span.start + 1, end: it.source.span.end - 1, text: self.to.to_string() });
        }
        walk::walk_import_declaration(self, it);
    }
    // acorn's `ExportNamedDeclaration` WITH a source (`export {a} from 'x'`) — oxc's own separate
    // `ExportFromDeclaration` type (see parse-js.rs's module doc for the 3-way split).
    fn visit_export_from_declaration(&mut self, it: &ExportFromDeclaration<'a>) {
        if it.source.value.as_str() == self.from {
            self.edits.push(Edit { start: it.source.span.start + 1, end: it.source.span.end - 1, text: self.to.to_string() });
        }
        walk::walk_export_from_declaration(self, it);
    }
    fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
        if it.source.value.as_str() == self.from {
            self.edits.push(Edit { start: it.source.span.start + 1, end: it.source.span.end - 1, text: self.to.to_string() });
        }
        walk::walk_export_all_declaration(self, it);
    }
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Expression::Identifier(id) = &it.callee {
            if id.name.as_str() == "require" {
                if let Some(Argument::StringLiteral(sl)) = it.arguments.first() {
                    if sl.value.as_str() == self.from {
                        self.edits.push(Edit { start: sl.span.start + 1, end: sl.span.end - 1, text: self.to.to_string() });
                    }
                }
            }
        }
        walk::walk_call_expression(self, it);
    }
}

fn import_edits(program: &Program, from: &str, to: &str) -> Vec<Edit> {
    let mut ic = ImportEditCollector { from, to, edits: Vec::new() };
    ic.visit_program(program);
    ic.edits
}

pub fn codeTransform(input: &Value, db: &Connection) -> Result<Value, String> {
    let file = match input.get("file").and_then(Value::as_str) {
        Some(f) if !f.is_empty() => f.to_string(),
        _ => return Ok(json!({ "ok": false, "error": "no file" })),
    };
    let op = input.get("op").and_then(Value::as_str).unwrap_or("").to_string();
    let args = input.get("args").cloned().unwrap_or_else(|| json!({}));
    let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_string();

    let before = match crate::codebase_codebase_file_read::codebaseFileRead(&json!({ "path": file }), db) {
        Ok(r) => r.get("content").and_then(Value::as_str).unwrap_or("").to_string(),
        Err(e) => {
            let msg: String = e.chars().take(120).collect();
            return Ok(json!({ "ok": false, "file": file, "error": format!("unreadable: {msg}") }));
        }
    };
    if before.trim().is_empty() {
        return Ok(json!({ "ok": false, "file": file, "error": "file is empty" }));
    }

    let allocator = Allocator::default();
    let source_type = SourceType::jsx();
    let ret = Parser::new(&allocator, &before, source_type)
        .with_options(ParseOptions { preserve_parens: false, ..ParseOptions::default() })
        .parse();
    if ret.diagnostics.has_errors() {
        let msg = ret.diagnostics.errors().next().map(|d| d.to_string()).unwrap_or_default();
        let msg: String = msg.chars().take(120).collect();
        return Ok(json!({ "ok": false, "file": file, "error": format!("couldn't parse (JS/ESM only for now): {msg}") }));
    }
    let program = ret.program;

    let edits = if op == "rename" {
        let (from, to) = (arg("from"), arg("to"));
        if from.is_empty() || to.is_empty() {
            return Ok(json!({ "ok": false, "file": file, "error": "rename needs { from, to }" }));
        }
        rename_edits(&program, &from, &to)
    } else if op == "rewire_import" {
        let (from, to) = (arg("from"), arg("to"));
        if from.is_empty() || to.is_empty() {
            return Ok(json!({ "ok": false, "file": file, "error": "rewire_import needs { from, to }" }));
        }
        import_edits(&program, &from, &to)
    } else {
        return Ok(json!({ "ok": false, "file": file, "error": format!("unknown op '{op}'") }));
    };

    if edits.is_empty() {
        return Ok(json!({ "ok": false, "file": file, "op": op, "error": format!("nothing to change — '{}' not found in {file}", arg("from")) }));
    }

    let changes = edits.len();
    let after = apply_edits(&before, edits);
    Ok(json!({ "ok": true, "file": file, "op": op, "before": before, "after": after, "changes": changes }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_schema(c: &Connection) {
        c.execute_batch("CREATE TABLE workspaces (root_path TEXT, is_active INTEGER);").unwrap();
    }

    fn workspace_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("code-transform-test-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(c: &Connection, dir: &std::path::Path, name: &str, source: &str, input: Value) -> Value {
        std::fs::write(dir.join(name), source).unwrap();
        codeTransform(&input, c).unwrap()
    }

    fn setup(c: &Connection, dir: &std::path::Path) {
        seed_schema(c);
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", rusqlite::params![dir.to_string_lossy().to_string()]).unwrap();
    }

    #[test]
    fn no_file_errs() {
        let c = Connection::open_in_memory().unwrap();
        let r = codeTransform(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no file" }));
    }

    #[test]
    fn unreadable_file_errs() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let r = codeTransform(&json!({ "file": "nope.js" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().starts_with("unreadable:"));
    }

    #[test]
    fn empty_file_errs() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let r = run(&c, &dir, "empty.js", "  \n ", json!({ "file": "empty.js", "op": "rename", "args": { "from": "a", "to": "b" } }));
        assert_eq!(r, json!({ "ok": false, "file": "empty.js", "error": "file is empty" }));
    }

    #[test]
    fn syntax_error_reports_couldnt_parse() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let r = run(&c, &dir, "bad.js", "function ( { }\n", json!({ "file": "bad.js", "op": "rename", "args": { "from": "a", "to": "b" } }));
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().starts_with("couldn't parse (JS/ESM only for now):"));
    }

    #[test]
    fn unknown_op_errs() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let r = run(&c, &dir, "f.js", "const a = 1;\n", json!({ "file": "f.js", "op": "delete", "args": {} }));
        assert_eq!(r, json!({ "ok": false, "file": "f.js", "error": "unknown op 'delete'" }));
    }

    #[test]
    fn rename_needs_from_to() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let r = run(&c, &dir, "f.js", "const a = 1;\n", json!({ "file": "f.js", "op": "rename", "args": { "from": "a" } }));
        assert_eq!(r, json!({ "ok": false, "file": "f.js", "error": "rename needs { from, to }" }));
    }

    #[test]
    fn rename_nothing_found() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let r = run(&c, &dir, "f.js", "const a = 1;\n", json!({ "file": "f.js", "op": "rename", "args": { "from": "zzz", "to": "b" } }));
        assert_eq!(r, json!({ "ok": false, "file": "f.js", "op": "rename", "error": "nothing to change — 'zzz' not found in f.js" }));
    }

    #[test]
    fn rename_simple_identifier_all_refs() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let source = "function add(a, b) {\n  return a + b;\n}\nconst x = add(1, 2);\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "add", "to": "sum" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["changes"], json!(2));
        assert_eq!(r["after"], json!("function sum(a, b) {\n  return a + b;\n}\nconst x = sum(1, 2);\n"));
        assert_eq!(r["before"], json!(source));
    }

    #[test]
    fn rename_skips_member_property_object_key_and_shorthand() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        // Verified live against the real JS (see module doc): `obj.mul` (member property),
        // `{ mul: 1 }` (non-shorthand key), and `{ mul }` (shorthand — key/value share one span
        // start) are all untouched. Only the bare reference `mul(1,2)` gets renamed.
        let source = "const mul = (a, b) => a * b;\nconst r1 = mul(1, 2);\nconst r2 = obj.mul;\nconst r3 = { mul: 1 };\nconst r4 = { mul };\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "mul", "to": "multiply" } }));
        assert_eq!(r["ok"], json!(true));
        let after = r["after"].as_str().unwrap();
        assert!(after.contains("const multiply = (a, b) => a * b;"));
        assert!(after.contains("const r1 = multiply(1, 2);"));
        assert!(after.contains("const r2 = obj.mul;")); // untouched
        assert!(after.contains("const r3 = { mul: 1 };")); // untouched (non-shorthand key)
        assert!(after.contains("const r4 = { mul };")); // untouched (shorthand — never renamed)
    }

    #[test]
    fn rename_object_expression_non_shorthand_value_renames_key_untouched() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        // Verified live: `{ mul: other }` — the VALUE reference `other` renames normally, the KEY
        // `mul` is a different name so it's unaffected either way here.
        let source = "const other = 1;\nconst r3 = { mul: other };\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "other", "to": "else1" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["after"], json!("const else1 = 1;\nconst r3 = { mul: else1 };\n"));
    }

    #[test]
    fn rename_destructuring_shorthand_binding_untouched_but_usage_renamed() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        // Verified live: `const { mul } = obj;` — the shorthand BINDING is untouched (same
        // key/value-span-collision rule as object-expression shorthand); the later usage
        // `mul(1)` DOES rename (it's an ordinary IdentifierReference, unrelated to the pattern).
        let source = "const { mul } = obj;\nconst y = mul(1);\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "mul", "to": "multiply" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["after"], json!("const { mul } = obj;\nconst y = multiply(1);\n"));
    }

    #[test]
    fn rename_destructuring_non_shorthand_renames_bound_value_key_untouched() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        // Verified live: `const { mul: renamed } = obj;` — the bound VALUE `renamed` (and its
        // later usage) rename normally; the KEY `mul` is untouched.
        let source = "const { mul: renamed } = obj;\nconst y = renamed(1);\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "renamed", "to": "newname" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["changes"], json!(2));
        assert_eq!(r["after"], json!("const { mul: newname } = obj;\nconst y = newname(1);\n"));
    }

    #[test]
    fn rename_export_specifier_keeps_public_name() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let source = "const z = 1;\nexport { z };\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "z", "to": "zz" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["after"], json!("const zz = 1;\nexport { zz as z };\n"));
    }

    #[test]
    fn rename_export_specifier_public_name_equals_new_name_drops_as() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let source = "const z = 1;\nexport { z as pub };\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "z", "to": "pub" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["after"], json!("const pub = 1;\nexport { pub };\n"));
    }

    #[test]
    fn rename_import_specifier_keeps_imported_name() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let source = "import { helper } from './lib.js';\nconst x = helper(1);\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "helper", "to": "assist" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["after"], json!("import { helper as assist } from './lib.js';\nconst x = assist(1);\n"));
    }

    #[test]
    fn rename_import_specifier_imported_name_equals_new_name_drops_as() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let source = "import { helper as h } from './lib.js';\nconst x = h(1);\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "h", "to": "helper" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["after"], json!("import { helper } from './lib.js';\nconst x = helper(1);\n"));
    }

    #[test]
    fn rename_class_method_key_renames_but_call_site_property_stays() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        // Verified live: a class MethodDefinition key is a DIFFERENT node type than `Property`
        // (never in the skip set), so it renames — but `w.mul(1,2)` is a non-computed
        // MemberExpression.property, always skipped. The mismatch is a genuine, faithfully
        // reproduced scope-naive quirk of this skill (see module doc).
        let source = "class Widget {\n  mul(a,b) { return a*b; }\n}\nconst w = new Widget();\nw.mul(1,2);\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "mul", "to": "multiply" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["changes"], json!(1));
        assert_eq!(r["after"], json!("class Widget {\n  multiply(a,b) { return a*b; }\n}\nconst w = new Widget();\nw.mul(1,2);\n"));
    }

    #[test]
    fn rename_computed_member_key_not_skipped() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        // `obj[key]` is a COMPUTED member access — the skip rule only ever applies to the
        // non-computed `StaticMemberExpression.property`, so this renames both occurrences.
        let source = "const key = 'x';\nconst v = obj[key];\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "key", "to": "k2" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["changes"], json!(2));
        assert_eq!(r["after"], json!("const k2 = 'x';\nconst v = obj[k2];\n"));
    }

    #[test]
    fn rename_renames_labels_too_scope_naive() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        // Verified live: acorn types a break/continue LABEL `Identifier` too, so this
        // scope-naive skill also renames labels — both the declaration and the `break` target.
        let source = "outer: for (let i=0;i<1;i++){ if(i) break outer; }\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rename", "args": { "from": "outer", "to": "loopA" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["changes"], json!(2));
        assert_eq!(r["after"], json!("loopA: for (let i=0;i<1;i++){ if(i) break loopA; }\n"));
    }

    #[test]
    fn rewire_import_needs_from_to() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let r = run(&c, &dir, "f.js", "import x from './a.js';\n", json!({ "file": "f.js", "op": "rewire_import", "args": { "from": "./a.js" } }));
        assert_eq!(r, json!({ "ok": false, "file": "f.js", "error": "rewire_import needs { from, to }" }));
    }

    #[test]
    fn rewire_import_declaration() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let source = "import { a, b } from './old.js';\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rewire_import", "args": { "from": "./old.js", "to": "./new.js" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["changes"], json!(1));
        assert_eq!(r["after"], json!("import { a, b } from './new.js';\n"));
    }

    #[test]
    fn rewire_export_from_and_export_all() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let source = "export { a } from './old.js';\nexport * from './old.js';\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rewire_import", "args": { "from": "./old.js", "to": "./new.js" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["changes"], json!(2));
        assert_eq!(r["after"], json!("export { a } from './new.js';\nexport * from './new.js';\n"));
    }

    #[test]
    fn rewire_require_call() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        let source = "const lib = require('./old.js');\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rewire_import", "args": { "from": "./old.js", "to": "./new.js" } }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["after"], json!("const lib = require('./new.js');\n"));
    }

    #[test]
    fn rewire_import_bare_export_specifier_untouched() {
        let c = Connection::open_in_memory().unwrap();
        let dir = workspace_dir();
        setup(&c, &dir);
        // bare `export { a };` has no `source` at all — must not match anything.
        let source = "const a = 1;\nexport { a };\n";
        let r = run(&c, &dir, "f.js", source, json!({ "file": "f.js", "op": "rewire_import", "args": { "from": "./old.js", "to": "./new.js" } }));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("nothing to change — './old.js' not found in f.js"));
    }
}
