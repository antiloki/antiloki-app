//! extract-anatomy.rs — twin of skills/audit/extract-anatomy.mjs. NAMES ARE CONTRACT:
//! `extractAnatomy` (default, standard db lane).
//!
//! ## Parser: native `oxc_parser`, parsed INLINE — but NO JSX (unlike parse-js.rs/file-internal-diagram.rs)
//! The JS source imports `{ parse } from 'acorn'` directly — plain acorn, no `acorn-jsx` plugin
//! (contrast parse-js.mjs, which explicitly extends acorn with `acorn-jsx`). `ACORN_OPTS =
//! { ecmaVersion: 'latest', sourceType: 'module' }` maps to `SourceType::mjs()` (JavaScript +
//! Module, NO JSX variant) — deliberately NOT `SourceType::jsx()`, the choice this crate's other
//! two oxc twins make. Since this skill walks `skills/**/*.mjs` (this codebase's OWN skill
//! sources, never a JSX-bearing UI file), the divergence is inert in practice but kept faithful to
//! the JS source's actual parser configuration. `preserve_parens: false` matches parse-js.rs's own
//! rationale (ESTree never wraps a parenthesized default-export expression in its own node).
//! Because the arena-allocated AST is lifetime-bound to its `Allocator`, parsing + the whole
//! top-level walk happen inline in `extract_from_ast`, same pattern as `parseJs`.
//!
//! ## `extractFromAst` only scans `ast.body` (Program top level) — no recursive walk needed
//! Unlike parse-js.rs/file-internal-diagram.rs (which need `oxc_ast_visit::Visit` for nested
//! call/identifier collection), this skill's `extractFromAst` loop never descends into function
//! bodies or nested expressions — it only classifies each top-level `Program` statement. So this
//! twin needs no `Visit` machinery at all, just a `match` over `Statement`/`Declaration`/
//! `ExportDefaultDeclarationKind` variants, mirroring parse-js.rs's/file-internal-diagram.rs's
//! documented ESTree-vs-oxc splits:
//! - **No single `Literal` node.** Acorn's `Literal` (boolean/null/numeric/bigint/regexp/string)
//!   collapses ALL SIX to acorn's node.type `'Literal'` — verified live (`export default 42;`,
//!   `export default 'hi';`, `export default null;` each print `decl.type === 'Literal'`). oxc
//!   splits these into six distinct `Expression` variants; `estree_type_name` below collapses them
//!   back to `"Literal"` for the `exportDefault` `else` branch's `decl?.type` string, matching acorn
//!   exactly (a real, verified divergence point — the JS source's own type-name STRINGS are part of
//!   its output shape here, not just structure).
//! - **`ExportNamedDeclaration` splits into three oxc types** (`ExportDeclaration` — inline
//!   `export function`/`export const`; `ExportNamedDeclaration` — bare `export {a,b};`;
//!   `ExportFromDeclaration` — `export {a,b} from 'x';`), exactly as parse-js.rs/
//!   file-internal-diagram.rs document. The JS source's own `if (declaration?.type ===
//!   'FunctionDeclaration') … else if (…VariableDeclaration) … else if (node.specifiers) …` becomes,
//!   in this twin: `Statement::ExportDeclaration` covers the first two arms (matching
//!   `ed.declaration`), and `Statement::ExportNamedDeclaration` / `Statement::ExportFromDeclaration`
//!   both cover the bare-specifiers arm (oxc's split of what's ONE acorn shape with an
//!   always-present, possibly-empty `.specifiers` array).
//! - **A top-level `export class Foo {}` produces NO named-export entry** — verified live: acorn's
//!   `node.declaration.type === 'ClassDeclaration'` matches neither the Function nor Variable arm,
//!   and `node.specifiers` is `[]` (present but empty) for THIS acorn node shape, so the specifiers
//!   loop runs zero times. In oxc's split this is simply: `Declaration::ClassDeclaration` (and any
//!   other non-Function/Variable inline declaration) falls to a no-op arm in
//!   `Statement::ExportDeclaration`'s match — never reaching the separate
//!   `ExportNamedDeclaration`/`ExportFromDeclaration` types at all (they don't carry an inline
//!   declaration in oxc). Same end result, reached via the type split rather than a runtime check.
//! - **A destructured top-level export/const declarator yields an entry with NO `name` key** —
//!   verified live (`export const { a, b } = x;` → `exports:[{"kind":"variable"}]`, the `name` key
//!   ABSENT, not `null`): acorn's `d.id.name` is `undefined` for an `ObjectPattern`/`ArrayPattern`
//!   binding, and `JSON.stringify` DROPS an undefined-valued object key entirely (vs. an explicit
//!   `?? null` elsewhere in the same file, which DOES serialize as `null`). This twin reproduces
//!   the drop-vs-null distinction by building each entry as a `serde_json::Map` and only inserting
//!   `"name"` when `Some`, vs. `default.name` which always inserts (`Value::Null` when absent) —
//!   see `push_named` vs. `function_default` below.
//! - **`exportDefault.params` maps EVERY non-plain-identifier param — including a `RestElement` —
//!   to the literal string `"<destructured>"`.** Verified live: `function f(a, ...rest)` →
//!   `params:["a","<destructured>"]`; `function f(a = 1)` → `params:["<destructured>"]`. The JS
//!   source does `p.name ?? '<destructured>'` on the RAW top-level param node (never recursing into
//!   `RestElement.argument` or `AssignmentPattern.left` the way parse-js.rs's `params_signature`
//!   does for the UNRELATED `parseJs` skill) — so a rest param's own missing `.name` hits the same
//!   fallback as a destructured/defaulted one. `params_list` below matches only oxc's
//!   `BindingPattern::BindingIdentifier`; every other pattern kind (including the separate
//!   `params.rest` field oxc keeps out-of-band — see parse-js.rs's own module doc on this split)
//!   maps to `"<destructured>"`.
//! - **Arrow functions always report `name: null`** (never omitted) — acorn's `ArrowFunctionExpression`
//!   has no `.id` at all (`decl.id` is `undefined`, `?? null` → explicit `null`), verified live.
//!   `function_default` takes `name: Option<&str>` and inserts it unconditionally (`json!` maps
//!   `None` → `Value::Null`), matching the `?? null` semantics — the opposite of the omitted-key
//!   case above.
//! - **A parse (syntax) error is NOT converted to any partial/best-effort result** — the JS source's
//!   `try { parsed = extractFromAst(source) } catch (e) { parseError = e.message }` discards
//!   whatever acorn built and falls back to the caller's defaults (`{imports:[], exports:[],
//!   default:null}`). `extract_from_ast` mirrors this: on `ret.diagnostics.has_errors()` it returns
//!   `Err(message)` before touching `program.body` at all, and the caller substitutes the same
//!   empty defaults. The exact ERROR TEXT is not byte-matched against acorn's `SyntaxError` message
//!   (acorn: `"Unexpected token (1:9)"` for `function (i {` verified live) — oxc's own diagnostic
//!   text is used instead, the same accepted divergence file-internal-diagram.rs documents for its
//!   own "couldn't parse" path (no fixture/frontier unit judges this string).
//!
//! ## BESPOKE-NATIVE, not conform-db fixture-tested (same class as load-skill.rs / catalog-refresh.rs)
//! This skill walks REAL directories via `crate::engine_load_skill::{listSkills, clearCache}`,
//! reads REAL files off disk (`readFileSync`/`existsSync`), AND queries SIX attached SQLite
//! schemas (`main`, `memory`, `brain`, `laws`, `knowledge`, `timeline` — the
//! FEAT-TIMELINE-RELOCATION comment in the JS source notes `sacred_timeline` moved to its own DB)
//! via `sqlite_master`/`PRAGMA table_info` introspection that has no fixture-seedable shape through
//! conform-db's two-lane harness. Proven below via `#[cfg(test)]` against a real temp directory
//! (reusing load-skill.rs's `ANTILOKI_SKILLS_ROOT` test seam, guarded by the same shared
//! `crate::test_env_lock::ENV_LOCK` since `listSkills`'s CACHE is process-global) plus a real
//! multi-schema in-memory `Connection`. `context.db` missing → JS throws
//! `err.contextMissing('extract-anatomy', 'db')` — unreachable here (`db: &Connection` is a
//! required parameter, not `Option`), matching catalog-refresh.rs's documented convention.
//!
//! `counts.sacred_timeline` queries the UNQUALIFIED table name `sacred_timeline` even though
//! `collectTables('timeline')` lists it as `timeline.sacred_timeline` — this works (in both the JS
//! `node:sqlite` connection and this twin's `rusqlite` one) because SQLite resolves an unqualified
//! table reference across `main` plus every ATTACHed database as long as the name is unambiguous;
//! `sacred_timeline` exists in exactly one attached schema. Ported literally (no schema prefix
//! added), verified against a real attached-db test below.
//!
//! `PRAGMA {schema.}table_info(name)` / `{schema.}sqlite_master` string-interpolate the table/
//! schema name with NO quoting, exactly like the JS source's own template literals — SQLite pragma
//! syntax doesn't accept bound parameters for identifiers, and both sides only ever see names
//! `sqlite_master` itself already returned, the same trust boundary the JS source relies on.
// QUALITY: genuine oxc port of the top-level-only AST scan (verified live against real acorn
// output across 12 shapes: literal/object/rest/assignment-pattern/arrow/class-export/destructured-
// const/bare-reexport/reexport-from/mixed/syntax-error defaults+exports) + real fs-walk + real
// multi-schema db introspection, all proven under #[cfg(test)].

use crate::engine_load_skill::{clearCache, listSkills};
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    BindingPattern, Declaration, ExportDefaultDeclarationKind, Expression, FormalParameters, Statement,
};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

// FEAT-TIMELINE-RELOCATION: sacred_timeline now lives in its own DB — see module doc.
const SCHEMAS: [&str; 6] = ["main", "memory", "brain", "laws", "knowledge", "timeline"];

/// `p.name ?? '<destructured>'` applied to a raw top-level param node — see module doc: this is
/// deliberately shallower than parse-js.rs's `params_signature` (no recursion into
/// `AssignmentPattern.left` / `RestElement.argument`), matching the JS source's own behavior: only
/// a plain identifier param yields its real name, every other pattern kind (and the separate
/// `params.rest` field oxc keeps out-of-band) falls back to `"<destructured>"`. Verified live:
/// acorn parses `function f(a = 1)`'s param as an `AssignmentPattern` node (no top-level `.name`)
/// → `["<destructured>"]`. oxc instead keeps the default on `FormalParameter.initializer` and
/// leaves `pattern` as the plain `BindingIdentifier` — so a param with an initializer must be
/// forced to `"<destructured>"` here even though `p.pattern` alone looks like a bare identifier.
fn params_list(params: &FormalParameters) -> Vec<Value> {
    let mut v: Vec<Value> = params
        .items
        .iter()
        .map(|p| match &p.pattern {
            BindingPattern::BindingIdentifier(id) if p.initializer.is_none() => json!(id.name.as_str()),
            _ => json!("<destructured>"),
        })
        .collect();
    if params.rest.is_some() {
        v.push(json!("<destructured>"));
    }
    v
}

/// Collapses an oxc `Expression` variant back to the acorn/ESTree `.type` STRING the JS source's
/// `decl?.type ?? 'unknown'` would have produced — see module doc for the verified `Literal`
/// collapse (acorn has ONE `Literal` type for 6 distinct value kinds).
fn estree_type_name(expr: &Expression) -> &'static str {
    match expr {
        Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::NumericLiteral(_)
        | Expression::BigIntLiteral(_)
        | Expression::RegExpLiteral(_)
        | Expression::StringLiteral(_) => "Literal",
        Expression::TemplateLiteral(_) => "TemplateLiteral",
        Expression::Identifier(_) => "Identifier",
        Expression::Super(_) => "Super",
        Expression::ArrayExpression(_) => "ArrayExpression",
        Expression::AssignmentExpression(_) => "AssignmentExpression",
        Expression::AwaitExpression(_) => "AwaitExpression",
        Expression::BinaryExpression(_) => "BinaryExpression",
        Expression::CallExpression(_) => "CallExpression",
        Expression::ChainExpression(_) => "ChainExpression",
        Expression::ClassExpression(_) => "ClassExpression",
        Expression::ConditionalExpression(_) => "ConditionalExpression",
        Expression::ImportExpression(_) => "ImportExpression",
        Expression::LogicalExpression(_) => "LogicalExpression",
        Expression::NewExpression(_) => "NewExpression",
        Expression::ObjectExpression(_) => "ObjectExpression",
        Expression::SequenceExpression(_) => "SequenceExpression",
        Expression::TaggedTemplateExpression(_) => "TaggedTemplateExpression",
        Expression::ThisExpression(_) => "ThisExpression",
        Expression::UnaryExpression(_) => "UnaryExpression",
        Expression::UpdateExpression(_) => "UpdateExpression",
        Expression::YieldExpression(_) => "YieldExpression",
        // acorn/ESTree types BOTH `import.meta` and `new.target` as a single `MetaProperty` node;
        // oxc splits them into two distinct variants — collapsed back here for the same reason
        // literals are collapsed above.
        Expression::ImportMeta(_) => "MetaProperty",
        Expression::NewTarget(_) => "MetaProperty",
        Expression::JSXElement(_) => "JSXElement",
        Expression::JSXFragment(_) => "JSXFragment",
        // FunctionExpression / ArrowFunctionExpression are intercepted before this fn is called.
        // ParenthesizedExpression is unreachable (preserve_parens:false); PrivateInExpression,
        // V8IntrinsicExpression (an oxc-only Node.js-intrinsic extension with no acorn/ESTree
        // counterpart), and the TS*Expression variants are unreachable under SourceType::mjs() (no
        // TypeScript) — same class of documented-unreachable branch as file-internal-diagram.rs's
        // TS arms.
        _ => "Unknown",
    }
}

fn function_default(name: Option<&str>, is_async: bool, params: &FormalParameters) -> Value {
    json!({
        "kind": "function",
        "name": name,
        "async": is_async,
        "params": params_list(params),
    })
}

/// `exportDefault` branch of `extractFromAst` — see module doc for the verified acorn shapes.
fn default_export_value(kind: &ExportDefaultDeclarationKind) -> Value {
    match kind {
        ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
            function_default(f.id.as_ref().map(|i| i.name.as_str()), f.r#async, &f.params)
        }
        ExportDefaultDeclarationKind::ClassDeclaration(_) => json!({ "kind": "ClassDeclaration" }),
        // TS-only; structurally unreachable under SourceType::mjs() (no TypeScript).
        ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => json!({ "kind": "TSInterfaceDeclaration" }),
        _ => match kind.to_expression() {
            Expression::FunctionExpression(fe) => {
                function_default(fe.id.as_ref().map(|i| i.name.as_str()), fe.r#async, &fe.params)
            }
            Expression::ArrowFunctionExpression(af) => function_default(None, af.r#async, &af.params),
            other => json!({ "kind": estree_type_name(other) }),
        },
    }
}

/// One `exportsNamed` entry — `name` is inserted ONLY when present (mirrors `JSON.stringify`
/// dropping an undefined-valued key; see module doc's destructured-declarator case), vs.
/// `function_default`'s `name` field, which is always inserted (`?? null` is an EXPLICIT null).
fn push_named(out: &mut Vec<Value>, name: Option<&str>, kind: &str) {
    let mut m = Map::new();
    if let Some(n) = name {
        m.insert("name".into(), json!(n));
    }
    m.insert("kind".into(), json!(kind));
    out.push(Value::Object(m));
}

#[derive(Debug)]
struct AstExtract {
    imports: Vec<String>,
    exports_named: Vec<Value>,
    default: Option<Value>,
}

/// `extractFromAst(source)` — parses INLINE (arena lifetime-bound), scans ONLY `ast.body` (no
/// recursive walk needed — see module doc). On a syntax error returns `Err(message)` instead of
/// any partial result, mirroring the JS source's try/catch discarding whatever acorn built.
/// `extractFromRust(source)` — the same three facts, read off RUST source.
///
/// The skill tree is Rust; `extract_from_ast` parses JavaScript and on a `.rs` file returns a
/// syntax error for every skill, which would report the whole tree as unparseable rather than as
/// what it is. These are the Rust spellings of the same structure:
///
///   imports  — `crate::<module>::`, the dependency edges (an `import` specifier's analogue).
///   exports  — `pub fn` / `pub struct` / `pub const`, the module's public surface.
///   default  — the contract function, i.e. the camelCase of the file name. Rust has no
///              `export default`; NAMES ARE CONTRACT is the convention that stands in for it, so
///              the "default" is whichever `pub fn` carries the skill's own name. The caller knows
///              that name, so this reports every public fn and lets the caller pick.
///
/// Line-scanned rather than parsed with `syn`: this needs declaration names, not semantics, and a
/// parser dependency to read `pub fn` off a line would not earn its weight.
fn extract_from_rust(source: &str) -> AstExtract {
    // Tests live inside the file; their `use super::*` is not a dependency of the SKILL.
    let body = match source.find("#[cfg(test)") {
        Some(i) => &source[..i],
        None => source,
    };
    let mut imports: Vec<String> = Vec::new();
    let mut exports_named: Vec<Value> = Vec::new();
    let mut default: Option<Value> = None;

    let needle = "crate::";
    let mut from = 0usize;
    while let Some(i) = body[from..].find(needle) {
        let start = from + i + needle.len();
        let end = body[start..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map(|e| start + e)
            .unwrap_or(body.len());
        let module = &body[start..end];
        if !module.is_empty() && !imports.iter().any(|m| m == module) {
            imports.push(module.to_string());
        }
        from = end;
    }

    for line in body.lines() {
        let t = line.trim_start();
        for (kw, kind) in [("pub fn ", "function"), ("pub struct ", "variable"), ("pub const ", "variable")] {
            if let Some(rest) = t.strip_prefix(kw) {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    push_named(&mut exports_named, Some(name.as_str()), kind);
                    if kind == "function" && default.is_none() {
                        default = Some(json!({ "kind": "function", "name": name }));
                    }
                }
            }
        }
    }
    AstExtract { imports, exports_named, default }
}

fn extract_from_ast(source: &str) -> Result<AstExtract, String> {
    let allocator = Allocator::default();
    // SourceType::mjs() = JavaScript + Module, NO JSX — see module doc: the JS source imports
    // plain `acorn`, not the `acorn-jsx`-extended parser parse-js.mjs uses.
    let source_type = SourceType::mjs();
    let ret = Parser::new(&allocator, source, source_type)
        .with_options(ParseOptions { preserve_parens: false, ..ParseOptions::default() })
        .parse();
    if ret.diagnostics.has_errors() {
        let msg = ret
            .diagnostics
            .errors()
            .next()
            .map(|d| d.to_string())
            .unwrap_or_else(|| "parse error".to_string());
        return Err(msg);
    }
    let program = ret.program;

    let mut imports: Vec<String> = Vec::new();
    let mut exports_named: Vec<Value> = Vec::new();
    let mut default: Option<Value> = None;

    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(imp) => {
                imports.push(imp.source.value.as_str().to_string());
            }
            // acorn's ExportNamedDeclaration WITH an inline `declaration` — see module doc.
            Statement::ExportDeclaration(ed) => match &ed.declaration {
                Declaration::FunctionDeclaration(f) => {
                    push_named(&mut exports_named, f.id.as_ref().map(|i| i.name.as_str()), "function");
                }
                Declaration::VariableDeclaration(vd) => {
                    for d in &vd.declarations {
                        let name = match &d.id {
                            BindingPattern::BindingIdentifier(id) => Some(id.name.as_str()),
                            _ => None,
                        };
                        push_named(&mut exports_named, name, "variable");
                    }
                }
                // e.g. `export class Foo {}` — matches neither JS branch, and acorn's own
                // `node.specifiers` is `[]` for this node shape too. No entry — verified live.
                _ => {}
            },
            // acorn's bare `export {a,b};` — oxc's specifiers-only split, see module doc.
            Statement::ExportNamedDeclaration(end) => {
                for s in &end.specifiers {
                    push_named(&mut exports_named, Some(s.exported.name().as_str()), "reexport");
                }
            }
            // acorn's `export {a,b} from 'x';` — oxc's specifiers+source split, see module doc.
            Statement::ExportFromDeclaration(efd) => {
                for s in &efd.specifiers {
                    push_named(&mut exports_named, Some(s.exported.name().as_str()), "reexport");
                }
            }
            Statement::ExportDefaultDeclaration(edd) => {
                default = Some(default_export_value(&edd.declaration));
            }
            _ => {}
        }
    }

    Ok(AstExtract { imports, exports_named, default })
}

fn scalar_count(db: &Connection, sql: &str) -> Result<i64, String> {
    db.query_row(sql, [], |row| row.get(0)).map_err(|e| format!("extract-anatomy: {e}"))
}

/// `collectTables(schemaName)` — see module doc for the no-quoting trust boundary.
fn collect_tables(db: &Connection, schema: &str) -> Result<Vec<Value>, String> {
    let sqlite_master = if schema == "main" { "sqlite_master".to_string() } else { format!("{schema}.sqlite_master") };
    let sql = format!("SELECT name FROM {sqlite_master} WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name");
    let mut stmt = db.prepare(&sql).map_err(|e| format!("extract-anatomy: {e}"))?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("extract-anatomy: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("extract-anatomy: {e}"))?;

    let prefix = if schema == "main" { String::new() } else { format!("{schema}.") };
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let qualified = if schema == "main" { name.clone() } else { format!("{schema}.{name}") };
        let pragma_sql = format!("PRAGMA {prefix}table_info({name})");
        let mut pstmt = db.prepare(&pragma_sql).map_err(|e| format!("extract-anatomy: {e}"))?;
        // PRAGMA table_info columns: cid(0), name(1), type(2), notnull(3), dflt_value(4), pk(5).
        let cols: Vec<Value> = pstmt
            .query_map([], |row| {
                let col_name: String = row.get(1)?;
                let col_type: String = row.get(2)?;
                let notnull: i64 = row.get(3)?;
                let pk: i64 = row.get(5)?;
                Ok(json!({ "name": col_name, "type": col_type, "notnull": notnull != 0, "pk": pk != 0 }))
            })
            .map_err(|e| format!("extract-anatomy: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("extract-anatomy: {e}"))?;
        out.push(json!({ "name": qualified, "columns": cols }));
    }
    Ok(out)
}

/// `collectViews(schemaName)`.
fn collect_views(db: &Connection, schema: &str) -> Result<Vec<String>, String> {
    let sqlite_master = if schema == "main" { "sqlite_master".to_string() } else { format!("{schema}.sqlite_master") };
    let sql = format!("SELECT name FROM {sqlite_master} WHERE type='view' ORDER BY name");
    let mut stmt = db.prepare(&sql).map_err(|e| format!("extract-anatomy: {e}"))?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("extract-anatomy: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("extract-anatomy: {e}"))?;
    Ok(names.into_iter().map(|name| if schema == "main" { name } else { format!("{schema}.{name}") }).collect())
}

/// `export default function extractAnatomy(_input, context)`. `context.db` missing is
/// unreachable here — see module doc.
#[allow(non_snake_case)]
pub fn extractAnatomy(_input: &Value, db: &Connection) -> Result<Value, String> {
    // ── Skills ──────────────────────────────────────────────────────────
    let mut listing = listSkills().as_array().cloned().unwrap_or_default();
    // If any cached path is stale (file created/deleted between calls), rebuild the index once.
    if listing.iter().any(|e| {
        let p = e.get("path").and_then(Value::as_str).unwrap_or("");
        !Path::new(p).exists()
    }) {
        clearCache();
        listing = listSkills().as_array().cloned().unwrap_or_default();
    }

    let mut skills: Vec<Value> = Vec::new();
    for entry in &listing {
        let name = entry.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let path = entry.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
        if !Path::new(&path).exists() {
            continue; // stale entry, skip silently
        }
        let source = std::fs::read_to_string(&path).map_err(|e| format!("extract-anatomy: {e}"))?;

        // Rust skills go through the Rust reader. Feeding a `.rs` file to the JS parser does not
        // fail gracefully — it records a syntax error for every skill in the tree, so the anatomy
        // would report the substrate as unparseable rather than as what it is.
        let (imports, exports_named, default, parse_error) = if path.ends_with(".rs") {
            let a = extract_from_rust(&source);
            (a.imports, a.exports_named, a.default, None)
        } else {
            match extract_from_ast(&source) {
                Ok(a) => (a.imports, a.exports_named, a.default, None),
                Err(msg) => (Vec::new(), Vec::new(), None, Some(msg)),
            }
        };

        let sha256 = {
            let mut hasher = Sha256::new();
            hasher.update(source.as_bytes());
            format!("{:x}", hasher.finalize())
        };
        let lines = source.split('\n').count() as i64;

        skills.push(json!({
            "name": name,
            "path": path.replace('\\', "/"),
            "lines": lines,
            "sha256": sha256,
            "parse_error": parse_error,
            "imports": imports,
            "exports": exports_named,
            "default": default,
        }));
    }
    skills.sort_by(|a, b| {
        a.get("name").and_then(Value::as_str).unwrap_or("").cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
    });

    // ── Schema: tables/views across all 6 attached databases ──────────────
    let mut tables: Vec<Value> = Vec::new();
    let mut views: Vec<String> = Vec::new();
    for schema in SCHEMAS {
        tables.extend(collect_tables(db, schema)?);
        views.extend(collect_views(db, schema)?);
    }
    tables.sort_by(|a, b| {
        a.get("name").and_then(Value::as_str).unwrap_or("").cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
    });
    views.sort();

    // ── Agents + actions (snapshot) ─────────────────────────────────────
    let agents: Vec<Value> = {
        let mut stmt = db
            .prepare("SELECT name, description, skills_json FROM agents ORDER BY name")
            .map_err(|e| format!("extract-anatomy: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                let name: String = row.get(0)?;
                let description: Option<String> = row.get(1)?;
                let skills_json: Option<String> = row.get(2)?;
                Ok((name, description, skills_json))
            })
            .map_err(|e| format!("extract-anatomy: {e}"))?;
        let mut out = Vec::new();
        for r in rows {
            let (name, description, skills_json) = r.map_err(|e| format!("extract-anatomy: {e}"))?;
            // `JSON.parse(a.skills_json || '[]').length` — invalid JSON is uncaught in the JS
            // source (would throw); mirrored here by propagating the parse error.
            let skill_count: i64 = match skills_json.as_deref().filter(|s| !s.is_empty()) {
                None => 0,
                Some(s) => {
                    let v: Value = serde_json::from_str(s).map_err(|e| format!("extract-anatomy: {e}"))?;
                    v.as_array().map(|a| a.len() as i64).unwrap_or(0)
                }
            };
            out.push(json!({ "name": name, "description": description, "skill_count": skill_count }));
        }
        out
    };

    let actions: Vec<Value> = {
        let mut stmt = db
            .prepare("SELECT name, owner_agent, description FROM actions ORDER BY name")
            .map_err(|e| format!("extract-anatomy: {e}"))?;
        let collected: Result<Vec<Value>, rusqlite::Error> = stmt.query_map([], |row| {
            let name: String = row.get(0)?;
            let owner_agent: Option<String> = row.get(1)?;
            let description: Option<String> = row.get(2)?;
            Ok(json!({ "name": name, "owner_agent": owner_agent, "description": description }))
        }).and_then(|rows| rows.collect());
        collected.map_err(|e| format!("extract-anatomy: {e}"))?
    };

    // ── Counts ──────────────────────────────────────────────────────────
    let counts = json!({
        "skills": skills.len() as i64,
        "tables": tables.len() as i64,
        "views": views.len() as i64,
        "agents": agents.len() as i64,
        "actions": actions.len() as i64,
        "cards": scalar_count(db, "SELECT COUNT(*) AS c FROM cards")?,
        // Unqualified — resolves via `timeline`, the sole attached schema holding this table. See module doc.
        "sacred_timeline": scalar_count(db, "SELECT COUNT(*) AS c FROM sacred_timeline")?,
        "memories": scalar_count(db, "SELECT COUNT(*) AS c FROM memory.memories")?,
        "embeddings": scalar_count(db, "SELECT COUNT(*) AS c FROM memory.embeddings")?,
        "memory_links": scalar_count(db, "SELECT COUNT(*) AS c FROM memory.memory_links")?,
        "memory_uses": scalar_count(db, "SELECT COUNT(*) AS c FROM memory.memory_uses")?,
        "memory_use_outcomes": scalar_count(db, "SELECT COUNT(*) AS c FROM memory.memory_use_outcomes")?,
        "brain_fires": scalar_count(db, "SELECT COUNT(*) AS c FROM brain.brain_fires")?,
        "brain_decisions": scalar_count(db, "SELECT COUNT(*) AS c FROM brain.brain_decisions")?,
        "brain_locks": scalar_count(db, "SELECT COUNT(*) AS c FROM brain.brain_locks")?,
        "laws": scalar_count(db, "SELECT COUNT(*) AS c FROM laws.laws")?,
        "law_clauses": scalar_count(db, "SELECT COUNT(*) AS c FROM laws.law_clauses")?,
        "law_links": scalar_count(db, "SELECT COUNT(*) AS c FROM laws.law_links")?,
        "law_violations": scalar_count(db, "SELECT COUNT(*) AS c FROM laws.law_violations")?,
        "documents": scalar_count(db, "SELECT COUNT(*) AS c FROM knowledge.documents")?,
        "extracts": scalar_count(db, "SELECT COUNT(*) AS c FROM knowledge.extracts")?,
    });

    // Intentionally omit generated_at — anatomy-snapshot's taken_at handles when. Stable output
    // enables sha256 dedup (no spurious hash diffs) — see module top comment in the JS source.
    Ok(json!({
        "counts": counts,
        "skills": skills,
        "tables": tables,
        "views": views,
        "agents": agents,
        "actions": actions,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // ── extract_from_ast: verified live against real acorn (see module doc) ──────────────

    #[test]
    fn literal_defaults_collapse_to_literal_kind() {
        for src in ["export default 42;", "export default 'hi';", "export default null;"] {
            let r = extract_from_ast(src).unwrap();
            assert!(r.imports.is_empty());
            assert!(r.exports_named.is_empty());
            assert_eq!(r.default, Some(json!({ "kind": "Literal" })));
        }
    }

    #[test]
    fn object_literal_default_is_object_expression_kind() {
        let r = extract_from_ast("export default { id: 1 };").unwrap();
        assert_eq!(r.default, Some(json!({ "kind": "ObjectExpression" })));
    }

    #[test]
    fn rest_param_maps_to_destructured_placeholder() {
        let r = extract_from_ast("export default function f(a, ...rest) { return 1; }").unwrap();
        assert_eq!(
            r.default,
            Some(json!({ "kind": "function", "name": "f", "async": false, "params": ["a", "<destructured>"] }))
        );
    }

    #[test]
    fn assignment_pattern_param_maps_to_destructured_placeholder() {
        let r = extract_from_ast("export default function f(a = 1) { return a; }").unwrap();
        assert_eq!(
            r.default,
            Some(json!({ "kind": "function", "name": "f", "async": false, "params": ["<destructured>"] }))
        );
    }

    #[test]
    fn arrow_default_has_null_name_not_omitted() {
        let r = extract_from_ast("export default (a, b) => a + b;").unwrap();
        assert_eq!(
            r.default,
            Some(json!({ "kind": "function", "name": null, "async": false, "params": ["a", "b"] }))
        );
    }

    #[test]
    fn async_arrow_default_reports_async_true() {
        let r = extract_from_ast("export default async (a) => a;").unwrap();
        assert_eq!(r.default.unwrap()["async"], json!(true));
    }

    #[test]
    fn class_export_produces_no_named_export_entry() {
        let r = extract_from_ast("export class Foo {}").unwrap();
        assert!(r.exports_named.is_empty());
        assert!(r.default.is_none());
    }

    #[test]
    fn destructured_const_export_entry_omits_name_key() {
        let r = extract_from_ast("export const { a, b } = x;").unwrap();
        assert_eq!(r.exports_named, vec![json!({ "kind": "variable" })]);
        assert!(r.exports_named[0].get("name").is_none());
    }

    #[test]
    fn bare_reexport_specifier() {
        let r = extract_from_ast("const z = 1; export { z };").unwrap();
        assert_eq!(r.exports_named, vec![json!({ "name": "z", "kind": "reexport" })]);
    }

    #[test]
    fn reexport_from_source_specifiers() {
        let r = extract_from_ast("export { six, seven } from '../numbers.js';").unwrap();
        assert_eq!(
            r.exports_named,
            vec![json!({ "name": "six", "kind": "reexport" }), json!({ "name": "seven", "kind": "reexport" })]
        );
    }

    #[test]
    fn mixed_import_named_function_and_default_function() {
        let src = "import { helper } from '../lib.mjs';\n\nexport function greet(name) {\n  return helper(name);\n}\n\nexport default function alpha(input, context) {\n  return greet(input.name);\n}\n";
        let r = extract_from_ast(src).unwrap();
        assert_eq!(r.imports, vec!["../lib.mjs".to_string()]);
        assert_eq!(r.exports_named, vec![json!({ "name": "greet", "kind": "function" })]);
        assert_eq!(
            r.default,
            Some(json!({ "kind": "function", "name": "alpha", "async": false, "params": ["input", "context"] }))
        );
    }

    #[test]
    fn syntax_error_returns_err_not_partial_result() {
        let err = extract_from_ast("function (i {").unwrap_err();
        assert!(!err.is_empty());
    }

    // ── extractAnatomy end-to-end: real fs walk + real multi-schema db ────────────────────

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-extract-anatomy-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_skills_root<T>(root: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SKILLS_ROOT", root.to_str().unwrap());
        }
        clearCache();
        let result = f();
        clearCache();
        unsafe {
            std::env::remove_var("ANTILOKI_SKILLS_ROOT");
        }
        result
    }

    fn db_full_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             ATTACH ':memory:' AS brain;
             ATTACH ':memory:' AS laws;
             ATTACH ':memory:' AS knowledge;
             ATTACH ':memory:' AS timeline;
             CREATE TABLE cards (id INTEGER PRIMARY KEY);
             CREATE TABLE agents (name TEXT, description TEXT, skills_json TEXT);
             CREATE TABLE actions (name TEXT, owner_agent TEXT, description TEXT);
             CREATE VIEW open_cards AS SELECT * FROM cards;
             CREATE TABLE timeline.sacred_timeline (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.embeddings (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.memory_links (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.memory_uses (id INTEGER PRIMARY KEY);
             CREATE TABLE memory.memory_use_outcomes (id INTEGER PRIMARY KEY);
             CREATE TABLE brain.brain_fires (id INTEGER PRIMARY KEY);
             CREATE TABLE brain.brain_decisions (id INTEGER PRIMARY KEY);
             CREATE TABLE brain.brain_locks (id INTEGER PRIMARY KEY);
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY);
             CREATE TABLE laws.law_clauses (id INTEGER PRIMARY KEY);
             CREATE TABLE laws.law_links (id INTEGER PRIMARY KEY);
             CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY);
             CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY);
             CREATE TABLE knowledge.extracts (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        c
    }

    #[test]
    fn extract_anatomy_end_to_end_over_real_skills_and_schema() {
        let dir = temp_dir("e2e");
        // RUST fixtures, because the tree is Rust. Note what is NOT here: a "broken" counterpart.
        // extract_from_rust is a line scan with no syntax to fail on, so a Rust skill cannot make
        // the anatomy report a parse error — the JS parser's failure mode has no analogue.
        fs::write(
            dir.join("alpha.rs"),
            "//! alpha.rs — the fixture skill.
use crate::engine_load_skill::listSkills;

pub fn alpha(input: &Value) {}
pub struct Helper;
",
        )
        .unwrap();

        with_skills_root(&dir, || {
            let db = db_full_schema();
            db.execute(
                "INSERT INTO agents (name, description, skills_json) VALUES ('dev', 'dev agent', '[\"alpha\"]')",
                [],
            )
            .unwrap();
            db.execute(
                "INSERT INTO actions (name, owner_agent, description) VALUES ('dev.alpha.run', 'dev', 'runs alpha')",
                [],
            )
            .unwrap();
            db.execute("INSERT INTO cards DEFAULT VALUES", []).unwrap();

            let result = extractAnatomy(&Value::Null, &db).unwrap();

            let skills = result["skills"].as_array().unwrap();
            assert_eq!(skills.len(), 1);
            assert_eq!(skills[0]["name"], json!("alpha"));

            let alpha = &skills[0];
            assert_eq!(alpha["parse_error"], Value::Null);
            // `crate::<module>::` is the Rust dependency edge — the import specifier's analogue.
            assert_eq!(alpha["imports"], json!(["engine_load_skill"]));
            // The public surface: the contract fn plus the struct.
            assert_eq!(
                alpha["exports"],
                json!([{ "name": "alpha", "kind": "function" }, { "name": "Helper", "kind": "variable" }]),
            );
            assert_eq!(alpha["default"]["kind"], json!("function"));
            assert_eq!(alpha["default"]["name"], json!("alpha"));
            assert!(alpha["sha256"].as_str().unwrap().len() == 64);

            assert_eq!(result["counts"]["skills"], json!(1));
            assert_eq!(result["counts"]["agents"], json!(1));
            assert_eq!(result["counts"]["actions"], json!(1));
            assert_eq!(result["counts"]["cards"], json!(1));
            assert_eq!(result["counts"]["sacred_timeline"], json!(0)); // unqualified cross-schema resolution

            let agents = result["agents"].as_array().unwrap();
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0]["name"], json!("dev"));
            assert_eq!(agents[0]["skill_count"], json!(1));

            let actions = result["actions"].as_array().unwrap();
            assert_eq!(actions[0]["name"], json!("dev.alpha.run"));
            assert_eq!(actions[0]["owner_agent"], json!("dev"));

            let tables = result["tables"].as_array().unwrap();
            let table_names: Vec<&str> = tables.iter().map(|t| t["name"].as_str().unwrap()).collect();
            assert!(table_names.contains(&"cards"));
            assert!(table_names.contains(&"agents"));
            assert!(table_names.contains(&"timeline.sacred_timeline"));
            assert!(table_names.contains(&"memory.memories"));

            let cards_table = tables.iter().find(|t| t["name"] == json!("cards")).unwrap();
            let cols = cards_table["columns"].as_array().unwrap();
            assert_eq!(cols[0]["name"], json!("id"));
            assert_eq!(cols[0]["pk"], json!(true));

            let views = result["views"].as_array().unwrap();
            assert_eq!(views, &vec![json!("open_cards")]);
        });

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_cached_path_triggers_rebuild_and_is_skipped() {
        let dir = temp_dir("stale");
        let file = dir.join("temp.mjs");
        fs::write(&file, "export default function temp() { return 1; }\n").unwrap();

        with_skills_root(&dir, || {
            let db = db_full_schema();
            // Prime the cache, then delete the file on disk without calling clearCache — the
            // cache now holds a stale path, forcing extractAnatomy's own rebuild-once branch.
            let _ = listSkills();
            fs::remove_file(&file).unwrap();

            let result = extractAnatomy(&Value::Null, &db).unwrap();
            let skills = result["skills"].as_array().unwrap();
            assert!(skills.is_empty());
            assert_eq!(result["counts"]["skills"], json!(0));
        });

        let _ = fs::remove_dir_all(&dir);
    }
}
