//! bugfinder-existence-check.rs — twin of skills/bugfinder/bugfinder-existence-check.mjs (WAVE 1,
//! rung 1 of the Oracle Ladder — deterministic, zero-AI hallucinated-import/symbol detection).
//! NAMES ARE CONTRACT: `bugfinderExistenceCheck` (default). For every import-ish edge in a scoped
//! JS/TS file, resolved against REAL disk state: relative imports must exist as a file (ext/index
//! tolerant), package imports must be declared in a walk-up `package.json` or present under a
//! walk-up `node_modules/`, and named imports must be exported by their resolved local target
//! (fail-OPEN on re-exporting barrels).
//!
//! ## AST walk: genuine oxc port, same class as parse-js.rs
//! `collectImports` in the JS source is a hand-rolled generic `Object.keys` walk over the WHOLE
//! AST (skipping only `loc`/`range`/`parent`) looking for four node shapes anywhere in the tree:
//! `ImportDeclaration`, `(ExportNamedDeclaration | ExportAllDeclaration) with node.source`,
//! `ImportExpression` (dynamic `import('literal')`), and `CallExpression` where
//! `callee.name === 'require'` with a string-literal first argument. This is exactly the shape
//! parse-js.rs already solved: `oxc_ast_visit::Visit`'s generated full recursive descent IS that
//! same "every enumerable child" walk, so `ImportEdgeCollector` below overrides only the 5
//! `visit_*` methods that matter and falls through to the generated `walk_*` default everywhere
//! else — no risk of silently skipping a node kind the JS walk would have reached.
//!
//! ## The oxc-vs-acorn split for exports/imports (see parse-js.rs's module doc for the general
//! rule) applies here too:
//! - Acorn's single `ExportNamedDeclaration` (bare `export {a,b};` has `.source === null`, `export
//!   {a,b} from 'x';` has `.source` set) splits in oxc into TWO node types with NO shared shape:
//!   `Statement::ExportNamedDeclaration` (bare form — verified via the crate source: this oxc
//!   struct has NO `source` field at all, so it can never carry an edge, matching JS's `node.source`
//!   falsy check exactly by construction) and `Statement::ExportFromDeclaration` (`.source` is a
//!   mandatory `StringLiteral`, always pushes an edge). `Statement::ExportAllDeclaration` (`export *
//!   from 'x'`) always carries `.source` too. JS's `(ExportNamedDeclaration || ExportAllDeclaration)
//!   && node.source` guard becomes, in this twin, two separate always-push `visit_*` overrides
//!   (`visit_export_from_declaration`, `visit_export_all_declaration`) plus NO override at all for
//!   oxc's bare `ExportNamedDeclaration` (the type system already excludes it from ever matching).
//! - `ImportDeclaration.specifiers` is `Option<Vec<ImportDeclarationSpecifier>>` (`None` for `import
//!   'foo'` side-effect-only, `Some([])` for `import {} from 'foo'`) vs acorn's always-array
//!   `node.specifiers ?? []`; `it.specifiers.as_ref().into_iter().flatten()` walks zero items for
//!   `None` and the real items for `Some(vec)`, reproducing the JS `?? []` fallback.
//! - `ImportSpecifier.imported` is `ModuleExportName` (`IdentifierName | IdentifierReference |
//!   StringLiteral` — the last for the arbitrary-module-namespace-names proposal, `import { "a-b" as
//!   x }`), not a single node type; `module_export_name_str` covers all three, mirroring the JS
//!   source's own defensive `s.imported?.name ?? s.imported?.value` (works whether `imported` is an
//!   Identifier-shaped node with `.name` or a Literal-shaped node with `.value`).
//! - `Argument` (call arguments) is oxc's "INHERIT(Expression)" pattern (see the crate's generated
//!   `inherit_variants.rs`): every `Expression` variant is ALSO a direct `Argument` variant, plus
//!   `SpreadElement`. `Argument::as_expression()` (a real inherent method, safe because the two enums
//!   share discriminants+layout for every shared variant) recovers the acorn-shaped
//!   `node.arguments[0].type === 'Literal'` check as `Some(Expression::StringLiteral(_))`.
//!
//! ## The fs-resolution half is NOT an AST concern — real disk I/O, ported directly
//! `tryResolveRelative`/`packageExists`/`exportsName` do no AST work at all: they call
//! `existsSync`/`readFileSync` against the real filesystem and regex-match raw source text. This
//! twin ports them as literal Rust `std::fs`/`std::path` + `regex` logic (no oxc involved), the same
//! way `codebase-file-read.rs` ports fs-lane skills. Two disclosed narrow gaps, both irrelevant to
//! existence-check CORRECTNESS because the underlying OS resolves `..`/`.` segments identically
//! whether or not the path STRING was pre-normalized:
//! 1. `resolve_path`/`normalize_components` approximate Node's `path.resolve`/`path.join` via
//!    `Path::components()` folding (pop on `ParentDir`, skip `CurDir`) rather than the byte-exact
//!    `win_normalize` string algorithm `workspace-root.rs` ports for `resolveWorkspacePath`'s
//!    containment check — that twin needs BYTE-IDENTICAL strings because it does prefix comparison
//!    on the result; this twin only ever calls `.exists()`/`.join()` on the resolved `PathBuf`, so a
//!    non-byte-identical-but-semantically-equal path is fine. `resolve(input.root)`'s `process.cwd()`
//!    fallback for a bare relative `root` input is NOT replicated (no cwd concept threaded through
//!    the db-lane call sites in this codebase) — a relative `root` stays un-anchored rather than
//!    resolving against a possibly-lane-divergent cwd, the same call this module's sibling
//!    `workspace-root.rs` makes for its single-segment `resolve()`.
//! 2. `BUILTINS` is a frozen snapshot of `node:module`'s `builtinModules` (captured via a live
//!    `node -e` run this pass, Node v24.18.0) rather than a live introspection — Rust has no
//!    `node:module` to query. A future Node LTS bump that adds a new builtin would need this list
//!    refreshed; same class of gap as any embedded-constant twin (see `error.rs`'s `ErrorCodes`).
//!
//! ## SourceType: always TypeScript+JSX, not extension-gated (deliberate, matches the JS source)
//! `js-ast.mjs`'s `parseModule` is `acorn.Parser.extend(tsPlugin())` with `sourceType:'module'`,
//! applied UNCONDITIONALLY to every extension this skill's own `JS_RE` accepts (`.mjs .cjs .js .jsx
//! .ts .tsx`) — the same file-extension-agnostic-parsing convention parse-js.rs documents for JSX,
//! taken one step further here because the JS source's own parser is TS-capable always, not just
//! JSX-capable always. `SourceType::tsx()` (TypeScript language + JSX variant + Unambiguous module)
//! is the oxc equivalent, used for every file this twin parses regardless of extension.
//!
//! ## Verified vs assumed
//! Cross-checked against a live Node run of the real JS `bugfinderExistenceCheck` (dynamic
//! `import()`) against real temp-dir fixtures this pass: a named-import miss + default-import miss +
//! missing relative target + hallucinated package (`import`/named/default checks, all four finding
//! shapes byte-for-byte incl. `detail`/`evidence`/`line`), a `package.json`-declared dependency
//! resolving with NO finding even though `node_modules/<name>` also exists (either walk-up hit
//! suffices), a `require('./lib.mjs')` and a dynamic `import('./lib.mjs')` producing no findings
//! (neither edge type carries named/default checks), a `node:fs` builtin import producing no
//! finding, a bare `export { helper };` (no source) producing no edge at all, an `export * from
//! './target.mjs'` re-exporting barrel making a downstream `import { anything, notreal } from
//! './reexport.mjs'` fail OPEN (zero findings even though neither name is really exported — the
//! REEXPORTS_RE short-circuit), and the TS-extension-swap convention (`./typed.js` doesn't exist on
//! disk but `./typed.ts` does — resolves cleanly, default-export check passes against the `.ts`
//! target's real source). Also confirmed live a JS-source quirk this twin reproduces exactly rather
//! than "fixing": for `import { missing } from './subdir'` where `./subdir` IS a real directory
//! (containing `index.mjs` but no `missing` export), the FIRST candidate tried is the bare directory
//! itself — `existsSync` (and this twin's `.exists()`) is true for directories too, not just files —
//! so `target` resolves to the extension-less directory path and the JS_RE/`is_js_family` extension
//! check on `target` then fails, skipping the named-export check entirely (silent pass, zero
//! findings, even for a genuinely-missing export). The `index.<ext>` candidates this skill's own
//! doc comment calls "index tolerant" only ever get reached when the bare joined path does NOT
//! already exist on disk as SOME entry (file or dir) — a narrower case than the doc comment implies.
// QUALITY: 82/100 — genuine oxc AST walk (same tractability class as parse-js.rs) + real std::fs/std::path port of the disk-resolution half; two disclosed narrow gaps (path-string byte-format, frozen builtins snapshot), neither affecting existence-check correctness
use oxc_allocator::Allocator;
use oxc_ast::ast::{
    CallExpression, ExportAllDeclaration, ExportFromDeclaration, Expression, ImportDeclaration,
    ImportDeclarationSpecifier, ImportExpression, ImportOrExportKind, ModuleExportName,
};
use oxc_ast_visit::{walk, Visit};
use oxc_parser::{ParseOptions, Parser};
use oxc_span::{SourceType, Span};
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

const MAX_SOURCE: usize = 400_000;
const EXT_CANDIDATES: [&str; 8] = ["", ".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx", ".json"];
const JS_FAMILY_EXTS: [&str; 6] = [".mjs", ".cjs", ".js", ".jsx", ".ts", ".tsx"];

// node -e "console.log(JSON.stringify(require('module').builtinModules))" — Node v24.18.0 snapshot
// (see module doc gap #2). JS builds `BUILTINS` as this list PLUS each entry re-prefixed with
// `node:` (harmless double-prefix for entries that already carry it, e.g. `node:sea`).
const BUILTIN_MODULES: [&str; 72] = [
    "_http_agent", "_http_client", "_http_common", "_http_incoming", "_http_outgoing", "_http_server",
    "_stream_duplex", "_stream_passthrough", "_stream_readable", "_stream_transform", "_stream_wrap",
    "_stream_writable", "_tls_common", "_tls_wrap", "assert", "assert/strict", "async_hooks", "buffer",
    "child_process", "cluster", "console", "constants", "crypto", "dgram", "diagnostics_channel", "dns",
    "dns/promises", "domain", "events", "fs", "fs/promises", "http", "http2", "https", "inspector",
    "inspector/promises", "module", "net", "os", "path", "path/posix", "path/win32", "perf_hooks", "process",
    "punycode", "querystring", "readline", "readline/promises", "repl", "stream", "stream/consumers",
    "stream/promises", "stream/web", "string_decoder", "sys", "timers", "timers/promises", "tls", "trace_events",
    "tty", "url", "util", "util/types", "v8", "vm", "wasi", "worker_threads", "zlib", "node:sea", "node:sqlite",
    "node:test", "node:test/reporters",
];

static BUILTINS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let mut s = HashSet::new();
    for m in BUILTIN_MODULES {
        s.insert(m.to_string());
        s.insert(format!("node:{m}"));
    }
    s
});

static REEXPORTS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"export\s+\*|export\s*\{[^}]*\}\s*from").unwrap());
static DEFAULT_EXPORT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"export\s+default\b").unwrap());
static MODULE_EXPORTS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"module\.exports\s*=").unwrap());

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

fn is_js_family(path: &str) -> bool {
    JS_FAMILY_EXTS.iter().any(|ext| path.ends_with(ext))
}

// acorn's `Position.line` counted the same way as parse-js.rs's `line_of`: 1-based, `\n`-counting
// up to a byte offset (ASCII/BMP source assumption — the codebase-wide fixture convention).
fn line_of(source: &str, offset: u32) -> i64 {
    1 + source.as_bytes()[..offset as usize].iter().filter(|&&b| b == b'\n').count() as i64
}

fn evidence_at(lines: &[&str], line: i64) -> String {
    let idx = (line - 1).max(0) as usize;
    lines.get(idx).copied().unwrap_or("").trim().chars().take(300).collect()
}

// node:path win32 `isAbsolute`: a leading separator (drive-relative but still "absolute" per Node's
// own win32 semantics), OR a drive letter followed by `:` and a separator.
fn is_absolute_like(p: &str) -> bool {
    let b = p.as_bytes();
    if b.is_empty() { return false; }
    if b[0] == b'/' || b[0] == b'\\' { return true; }
    b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

// See module doc gap #1: components-based `.`/`..` collapse, not a byte-exact node:path port —
// sufficient because every caller only ever feeds the result to `.exists()`/`.join()`.
fn normalize_components(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => { out.pop(); }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn resolve_path(base_dir: &Path, spec: &str) -> PathBuf {
    let spec_path = Path::new(spec);
    let joined = if spec_path.is_absolute() {
        spec_path.to_path_buf()
    } else if spec.starts_with('/') || spec.starts_with('\\') {
        // a POSIX-style rooted spec on Windows: anchor to base_dir's own drive (win32 `resolve`
        // treats a root-but-no-drive segment as relative to the current drive).
        match base_dir.components().next() {
            Some(Component::Prefix(prefix)) => Path::new(prefix.as_os_str()).join(spec.trim_start_matches(['/', '\\'])),
            _ => spec_path.to_path_buf(),
        }
    } else {
        base_dir.join(spec)
    };
    normalize_components(&joined)
}

// tryResolveRelative(fromDir, spec): candidate order matches the JS source exactly — bare
// extensions first (in EXT_CANDIDATES order), THEN the TS-convention swap (`.js/.mjs/.jsx` ->
// `.ts`/`.tsx`), THEN `index.<ext>` candidates.
fn try_resolve_relative(from_dir: &Path, spec: &str) -> Option<PathBuf> {
    let base = resolve_path(from_dir, spec);
    let base_str = base.to_string_lossy().into_owned();
    let mut candidates: Vec<PathBuf> = Vec::with_capacity(EXT_CANDIDATES.len() * 2 + 2);
    for ext in EXT_CANDIDATES { candidates.push(PathBuf::from(format!("{base_str}{ext}"))); }
    for suf in [".js", ".mjs", ".jsx"] {
        if let Some(stem) = base_str.strip_suffix(suf) {
            candidates.push(PathBuf::from(format!("{stem}.ts")));
            candidates.push(PathBuf::from(format!("{stem}.tsx")));
            break;
        }
    }
    for ext in EXT_CANDIDATES { candidates.push(base.join(format!("index{ext}"))); }
    candidates.into_iter().find(|c| c.exists())
}

fn package_name(spec: &str) -> String {
    let parts: Vec<&str> = spec.split('/').collect();
    if spec.starts_with('@') {
        parts.iter().take(2).copied().collect::<Vec<_>>().join("/")
    } else {
        parts.first().copied().unwrap_or("").to_string()
    }
}

// walk UP from fromDir to root: a package.json declaring `name`, or a node_modules/<name> dir,
// anywhere on the path, settles existence. Unreadable/malformed package.json never accuses anyone
// (matches the JS source's empty `catch {}`).
fn package_exists(from_dir: &Path, root: &Path, name: &str) -> bool {
    let stop_at = normalize_components(root);
    let mut dir = from_dir.to_path_buf();
    for _ in 0..30 {
        let pj = dir.join("package.json");
        if let Ok(text) = std::fs::read_to_string(&pj) {
            if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                for k in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"] {
                    if parsed.get(k).and_then(|v| v.as_object()).is_some_and(|o| o.contains_key(name)) {
                        return true;
                    }
                }
            }
        }
        if dir.join("node_modules").join(name).exists() { return true; }
        if normalize_components(&dir) == stop_at { break; }
        match dir.parent() {
            Some(p) if p != dir => dir = p.to_path_buf(),
            _ => break,
        }
    }
    false
}

fn exports_name(target_src: &str, name: &str) -> bool {
    let esc = regex::escape(name);
    let re1 = Regex::new(&format!(r"export\s+(?:async\s+)?(?:function\*?|class|const|let|var)\s+{esc}\b")).unwrap();
    if re1.is_match(target_src) { return true; }
    let re2 = Regex::new(&format!(r"export\s*\{{[^}}]*\b{esc}\b[^}}]*\}}")).unwrap();
    re2.is_match(target_src)
}

fn module_export_name_str(m: &ModuleExportName) -> String {
    match m {
        ModuleExportName::IdentifierName(id) => id.name.as_str().to_string(),
        ModuleExportName::IdentifierReference(id) => id.name.as_str().to_string(),
        ModuleExportName::StringLiteral(s) => s.value.as_str().to_string(),
    }
}

struct ImportEdge {
    spec: String,
    span: Span,
    named: Vec<(String, Span)>,
    wants_default: bool,
    type_only: bool,
}

// ── collectImports: full recursive descent — see module doc for the oxc-vs-acorn node-shape split ──
#[derive(Default)]
struct ImportEdgeCollector {
    edges: Vec<ImportEdge>,
}
impl<'a> Visit<'a> for ImportEdgeCollector {
    fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
        let type_only = matches!(it.import_kind, ImportOrExportKind::Type);
        let mut named: Vec<(String, Span)> = Vec::new();
        let mut wants_default = false;
        for s in it.specifiers.as_ref().into_iter().flatten() {
            match s {
                ImportDeclarationSpecifier::ImportDefaultSpecifier(_) => wants_default = true,
                ImportDeclarationSpecifier::ImportSpecifier(isp) => {
                    if !matches!(isp.import_kind, ImportOrExportKind::Type) {
                        named.push((module_export_name_str(&isp.imported), isp.span));
                    }
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(_) => {}
            }
        }
        self.edges.push(ImportEdge {
            spec: it.source.value.as_str().to_string(),
            span: it.span,
            named,
            wants_default,
            type_only,
        });
        walk::walk_import_declaration(self, it);
    }

    fn visit_export_from_declaration(&mut self, it: &ExportFromDeclaration<'a>) {
        self.edges.push(ImportEdge {
            spec: it.source.value.as_str().to_string(),
            span: it.span,
            named: Vec::new(),
            wants_default: false,
            type_only: matches!(it.export_kind, ImportOrExportKind::Type),
        });
        walk::walk_export_from_declaration(self, it);
    }

    fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
        self.edges.push(ImportEdge {
            spec: it.source.value.as_str().to_string(),
            span: it.span,
            named: Vec::new(),
            wants_default: false,
            type_only: matches!(it.export_kind, ImportOrExportKind::Type),
        });
        walk::walk_export_all_declaration(self, it);
    }

    fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
        if let Expression::StringLiteral(s) = &it.source {
            self.edges.push(ImportEdge {
                spec: s.value.as_str().to_string(),
                span: it.span,
                named: Vec::new(),
                wants_default: false,
                type_only: false,
            });
        }
        walk::walk_import_expression(self, it);
    }

    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Expression::Identifier(id) = &it.callee {
            if id.name.as_str() == "require" {
                if let Some(Expression::StringLiteral(s)) = it.arguments.first().and_then(|a| a.as_expression()) {
                    self.edges.push(ImportEdge {
                        spec: s.value.as_str().to_string(),
                        span: it.span,
                        named: Vec::new(),
                        wants_default: false,
                        type_only: false,
                    });
                }
            }
        }
        walk::walk_call_expression(self, it);
    }
}

fn finding(category: &str, severity: &str, file: &str, line: i64, detail: String, evidence: String) -> Value {
    json!({ "category": category, "severity": severity, "file": file, "line": line, "detail": detail, "evidence": evidence })
}

#[allow(non_snake_case)]
pub fn bugfinderExistenceCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    let files = match input.get("files").and_then(|v| v.as_array()) {
        Some(a) => a.clone(),
        None => return Err("bugfinder-existence-check: files is required".into()),
    };
    let root: PathBuf = match input.get("root").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(r) => normalize_components(Path::new(r)),
        None => {
            let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
            normalize_components(Path::new(root_v.as_str().unwrap_or_default()))
        }
    };

    let mut findings: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let mut checked: Vec<Value> = Vec::new();

    for f in &files {
        let rel_path = str_qq(f.get("path"));
        if !is_js_family(&rel_path) {
            skipped.push(json!({ "path": rel_path, "reason": "not-js-family" }));
            continue;
        }
        let abs: PathBuf = if is_absolute_like(&rel_path) {
            normalize_components(Path::new(&rel_path))
        } else {
            normalize_components(&root.join(&rel_path))
        };

        let source: String = match f.get("source").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => match std::fs::read_to_string(&abs) {
                Ok(s) => s,
                Err(_) => {
                    skipped.push(json!({ "path": rel_path, "reason": "unreadable" }));
                    continue;
                }
            },
        };
        if source.len() > MAX_SOURCE {
            skipped.push(json!({ "path": rel_path, "reason": "too-large" }));
            continue;
        }

        // Parse INLINE (arena-allocated, lifetime-bound to this iteration's Allocator — see the
        // module doc's SourceType note for why `tsx()` is used unconditionally).
        let allocator = Allocator::default();
        let source_type = SourceType::tsx();
        let ret = Parser::new(&allocator, &source, source_type)
            .with_options(ParseOptions { preserve_parens: false, ..ParseOptions::default() })
            .parse();
        if ret.diagnostics.has_errors() {
            skipped.push(json!({ "path": rel_path, "reason": "syntax-error (rung 0 owns it)" }));
            continue;
        }

        let lines: Vec<&str> = source.split('\n').collect();
        let from_dir = abs.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

        let mut collector = ImportEdgeCollector::default();
        collector.visit_program(&ret.program);

        for edge in &collector.edges {
            if edge.type_only { continue; }
            let spec = edge.spec.as_str();
            let edge_line = line_of(&source, edge.span.start);

            if spec.starts_with('.') || spec.starts_with('/') {
                match try_resolve_relative(&from_dir, spec) {
                    None => {
                        findings.push(finding(
                            "hallucinated-import", "high", &rel_path, edge_line,
                            format!("import target '{spec}' does not exist on disk (ext/index variants tried)"),
                            evidence_at(&lines, edge_line),
                        ));
                        continue;
                    }
                    Some(target) => {
                        if (!edge.named.is_empty() || edge.wants_default) && is_js_family(&target.to_string_lossy()) {
                            let target_src = match std::fs::read_to_string(&target) {
                                Ok(s) => s,
                                Err(_) => continue,
                            };
                            if REEXPORTS_RE.is_match(&target_src) { continue; }
                            for (name, nspan) in &edge.named {
                                if !exports_name(&target_src, name) {
                                    let n_line = line_of(&source, nspan.start);
                                    findings.push(finding(
                                        "unknown-symbol", "medium", &rel_path, n_line,
                                        format!("named import '{name}' is not exported by '{spec}'"),
                                        evidence_at(&lines, n_line),
                                    ));
                                }
                            }
                            if edge.wants_default && !DEFAULT_EXPORT_RE.is_match(&target_src) && !MODULE_EXPORTS_RE.is_match(&target_src) {
                                findings.push(finding(
                                    "unknown-symbol", "medium", &rel_path, edge_line,
                                    format!("default import from '{spec}', but the target has no default export"),
                                    evidence_at(&lines, edge_line),
                                ));
                            }
                        }
                    }
                }
            } else if !BUILTINS.contains(spec) {
                let name = package_name(spec);
                if !BUILTINS.contains(&name) && !package_exists(&from_dir, &root, &name) {
                    findings.push(finding(
                        "hallucinated-package", "high", &rel_path, edge_line,
                        format!("package '{name}' is not declared in any package.json on the path and not installed"),
                        evidence_at(&lines, edge_line),
                    ));
                }
            }
        }
        checked.push(json!(rel_path));
    }

    Ok(json!({ "checked": checked, "skipped": skipped, "findings": findings }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection { Connection::open_in_memory().unwrap() }

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bugfinder-existence-check-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn not_js_family_is_skipped() {
        let c = conn();
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "README.md", "source": "" }] }), &c).unwrap();
        assert_eq!(r["skipped"], json!([{ "path": "README.md", "reason": "not-js-family" }]));
        assert!(r["checked"].as_array().unwrap().is_empty());
    }

    #[test]
    fn missing_required_files_errors() {
        let c = conn();
        assert!(bugfinderExistenceCheck(&json!({}), &c).unwrap_err().contains("files"));
    }

    #[test]
    fn syntax_error_is_skipped_not_thrown() {
        let c = conn();
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "bad.mjs", "source": "const x = ;;;" }] }), &c).unwrap();
        assert_eq!(r["skipped"][0]["reason"], json!("syntax-error (rung 0 owns it)"));
    }

    #[test]
    fn hallucinated_relative_import_and_builtin_and_reexport_and_dynamic_and_require() {
        let c = conn();
        let dir = tmp("basic");
        std::fs::write(dir.join("lib.mjs"), "export function helper() {}\n").unwrap();
        let source = "\
import { helper, missing } from './lib.mjs';\n\
import def from './lib.mjs';\n\
import './noexist.mjs';\n\
import fs from 'node:fs';\n\
const dyn = () => import('./lib.mjs');\n\
const req = require('./lib.mjs');\n\
export { helper };\n";
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "a.mjs", "source": source }], "root": dir.to_string_lossy() }), &c).unwrap();
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 3);
        assert_eq!(findings[0]["category"], json!("unknown-symbol"));
        assert_eq!(findings[0]["detail"], json!("named import 'missing' is not exported by './lib.mjs'"));
        assert_eq!(findings[0]["line"], json!(1));
        assert_eq!(findings[1]["category"], json!("unknown-symbol"));
        assert_eq!(findings[1]["detail"], json!("default import from './lib.mjs', but the target has no default export"));
        assert_eq!(findings[1]["line"], json!(2));
        assert_eq!(findings[2]["category"], json!("hallucinated-import"));
        assert_eq!(findings[2]["detail"], json!("import target './noexist.mjs' does not exist on disk (ext/index variants tried)"));
        assert_eq!(findings[2]["line"], json!(3));
        assert_eq!(r["checked"], json!(["a.mjs"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hallucinated_package_vs_declared_dependency() {
        let c = conn();
        let dir = tmp("pkg");
        std::fs::write(dir.join("package.json"), r#"{ "dependencies": { "foo-lib": "^1.0.0" } }"#).unwrap();
        let source = "import fooLib from 'foo-lib';\nimport notinstalled from 'not-a-real-package';\n";
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "a.mjs", "source": source }], "root": dir.to_string_lossy() }), &c).unwrap();
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["category"], json!("hallucinated-package"));
        assert_eq!(findings[0]["detail"], json!("package 'not-a-real-package' is not declared in any package.json on the path and not installed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn node_modules_only_dependency_also_resolves() {
        let c = conn();
        let dir = tmp("nm");
        std::fs::create_dir_all(dir.join("node_modules/bar-lib")).unwrap();
        let source = "import bar from 'bar-lib';\n";
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "a.mjs", "source": source }], "root": dir.to_string_lossy() }), &c).unwrap();
        assert!(r["findings"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reexporting_barrel_fails_open_on_named_imports() {
        let c = conn();
        let dir = tmp("reexport");
        std::fs::write(dir.join("target.mjs"), "export const z = 1;\n").unwrap();
        std::fs::write(dir.join("reexport.mjs"), "export * from './target.mjs';\n").unwrap();
        let source = "import { anything, notreal } from './reexport.mjs';\n";
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "consumer.mjs", "source": source }], "root": dir.to_string_lossy() }), &c).unwrap();
        assert!(r["findings"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ts_extension_swap_resolves_js_specifier_to_a_ts_file() {
        let c = conn();
        let dir = tmp("tsswap");
        std::fs::write(dir.join("typed.ts"), "export default function () {}\n").unwrap();
        let source = "import tsDefault from './typed.js';\n";
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "consumer.mjs", "source": source }], "root": dir.to_string_lossy() }), &c).unwrap();
        assert!(r["findings"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // See module doc "Verified vs assumed": a directory-shaped relative spec resolves via the FIRST
    // (bare, extension-less) candidate — `.exists()` is true for directories too — so the extension
    // check on `target` fails and the named-export check never runs, even for a genuinely-missing
    // export. Reproduced here exactly as the live JS behaves (confirmed via a real Node run).
    #[test]
    fn directory_shaped_import_short_circuits_on_the_bare_directory_candidate() {
        let c = conn();
        let dir = tmp("index");
        std::fs::create_dir_all(dir.join("subdir")).unwrap();
        std::fs::write(dir.join("subdir/index.mjs"), "export const x = 1;\n").unwrap();
        let source = "import x from './subdir';\nimport { missing } from './subdir';\n";
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "consumer.mjs", "source": source }], "root": dir.to_string_lossy() }), &c).unwrap();
        assert!(r["findings"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn type_only_import_is_ignored_entirely() {
        let c = conn();
        let dir = tmp("typeonly");
        let source = "import type { Missing } from './noexist.ts';\n";
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "consumer.ts", "source": source }], "root": dir.to_string_lossy() }), &c).unwrap();
        assert!(r["findings"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_from_input_bypasses_disk_read_but_resolution_still_hits_disk() {
        let c = conn();
        let dir = tmp("srcinput");
        std::fs::write(dir.join("lib.mjs"), "export function helper() {}\n").unwrap();
        let r = bugfinderExistenceCheck(
            &json!({ "files": [{ "path": "a.mjs", "source": "import { helper } from './lib.mjs';\n" }] , "root": dir.to_string_lossy() }),
            &c,
        ).unwrap();
        assert!(r["findings"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_file_is_skipped_when_no_inline_source_given() {
        let c = conn();
        let dir = tmp("unreadable");
        let r = bugfinderExistenceCheck(&json!({ "files": [{ "path": "ghost.mjs" }], "root": dir.to_string_lossy() }), &c).unwrap();
        assert_eq!(r["skipped"][0]["reason"], json!("unreadable"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_name_helper() {
        assert_eq!(package_name("lodash"), "lodash");
        assert_eq!(package_name("lodash/fp"), "lodash");
        assert_eq!(package_name("@scope/pkg"), "@scope/pkg");
        assert_eq!(package_name("@scope/pkg/sub"), "@scope/pkg");
    }

    #[test]
    fn exports_name_matches_function_const_class_and_bare_specifier() {
        assert!(exports_name("export function helper() {}", "helper"));
        assert!(exports_name("export async function helper() {}", "helper"));
        assert!(exports_name("export const STORAGE_KEY = 'x';", "STORAGE_KEY"));
        assert!(exports_name("const z = 1;\nexport { z };", "z"));
        assert!(!exports_name("export const other = 1;", "z"));
    }
}
