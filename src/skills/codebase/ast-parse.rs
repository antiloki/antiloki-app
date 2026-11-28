//! ast-parse.rs — twin of skills/codebase/ast-parse.mjs, the language-AGNOSTIC parse dispatcher.
//! NAMES ARE CONTRACT: `adapterFor` · `supportedExtensions` · `astParse` (default, async in JS —
//! awaited to its resolved value per the async-default-export lane; this twin is an ordinary sync
//! `pub fn`, ignoring the async/await keywords per the playbook's async-lane rule).
//!
//! ## Adapter table
//! Transcribed verbatim from the JS source's `ADAPTERS` array (same order — `adapterFor` returns
//! the FIRST entry whose `exts` contains the file's lowercased extension, exactly like
//! `Array.prototype.find`). Each entry maps to either the JS/JSX lane (`grammar: None`, delegates
//! to `crate::codebase_parse_js::parseJs`) or a tree-sitter grammar name (`grammar: Some(name)`,
//! delegates to `crate::codebase_parse_treesitter::parseTreeSitter`).
//!
//! ## Scope + the PARITY-DEFERRED grammars
//! `parse-treesitter.rs` (this pass's other dependency) only ports the `tsx`/`typescript`/
//! `python` grammars — every other bundled grammar (go, java, c_sharp, cpp, c, php, rust, ruby,
//! kotlin, swift, scala, lua, elixir, bash) is scoped out THERE and returns `Value::Null`. The
//! real JS `parseTreeSitter` never returns null for those — it always produces a real extract via
//! `genericWalk` (or a precise profile for go/elixir/bash/lua). This twin does NOT attempt to
//! replicate `genericWalk`: when the underlying `parseTreeSitter` call returns `Value::Null`,
//! `astParse` returns `Value::Null` straight through — a DELIBERATE, DOCUMENTED divergence from
//! the real JS behavior for those languages only. No fixture case exercises this branch (per the
//! task scope: JS/JSX via parse-js.mjs, TS/TSX/Python via parse-treesitter.mjs — the two languages
//! both converted dependencies actually implement).
//!
//! ## The JSX-in-.js fallback (`parseJsWithJsxFallback`) is structurally unreachable here
//! The JS source wraps `parseJs` in a try/catch: acorn throws on a syntax error, and the catch
//! retries with the `tsx` tree-sitter grammar (a JSX superset) before giving up. `parse-js.rs`'s
//! `parseJs` (native `oxc_parser`, `SourceType::jsx()`) already parses JSX unconditionally in ANY
//! `.js`/`.mjs`/`.cjs` file — the exact case the JS fallback exists for — and, unlike acorn, oxc's
//! parser never fails to produce a `Program` for the inputs this twin's fixtures exercise (valid
//! source, matching the playbook's "fixture only the success path" rule). So this twin calls
//! `parseJs` directly for the JS/JSX lane with no try/catch: the fallback branch is dead code here
//! by construction, not an omitted feature — see `parse-js.rs`'s own module doc for the full
//! architecture note.
//!
//! ## `adapterFor` is `class:"transitive"`
//! The real JS `adapterFor` returns the matched `{langs, exts, parse}` object — `parse` is a
//! closure, unportable to JSON (same category of divergence as `contract-templates.rs`'s
//! `getTemplate`/`contractTemplates`, see that module's doc). This twin's `adapterFor` returns the
//! real `{langs, exts}` projection (dropping `parse`) — real logic, `#[cfg(test)]`-covered, but no
//! conform-db fixture (the harness's key-set/value comparison could never match the real 3-key,
//! function-carrying JS return).
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

// node:path's extname: from the last '.' in the last path segment to end, lowercased; a leading
// dot on the basename itself (a dotfile like ".gitignore") does NOT count as an extension.
fn extname(path: &str) -> String {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match base.rfind('.') {
        Some(0) | None => String::new(),
        Some(idx) => base[idx..].to_lowercase(),
    }
}

struct AdapterEntry {
    langs: &'static [&'static str],
    exts: &'static [&'static str],
    grammar: Option<&'static str>, // None = the JS/JSX (acorn/oxc) lane
}

// Verbatim order from the JS source's ADAPTERS array — see module doc.
const ADAPTERS: &[AdapterEntry] = &[
    AdapterEntry { langs: &["javascript"], exts: &[".js", ".mjs", ".cjs"], grammar: None },
    AdapterEntry { langs: &["typescript"], exts: &[".ts", ".mts", ".cts"], grammar: Some("typescript") },
    AdapterEntry { langs: &["tsx"], exts: &[".tsx"], grammar: Some("tsx") },
    AdapterEntry { langs: &["jsx"], exts: &[".jsx"], grammar: Some("tsx") },
    AdapterEntry { langs: &["python"], exts: &[".py"], grammar: Some("python") },
    AdapterEntry { langs: &["go"], exts: &[".go"], grammar: Some("go") },
    AdapterEntry { langs: &["java"], exts: &[".java"], grammar: Some("java") },
    AdapterEntry { langs: &["c_sharp"], exts: &[".cs"], grammar: Some("c_sharp") },
    AdapterEntry { langs: &["cpp"], exts: &[".cpp", ".cc", ".cxx", ".hpp", ".hh"], grammar: Some("cpp") },
    AdapterEntry { langs: &["c"], exts: &[".c", ".h"], grammar: Some("c") },
    AdapterEntry { langs: &["php"], exts: &[".php"], grammar: Some("php") },
    AdapterEntry { langs: &["rust"], exts: &[".rs"], grammar: Some("rust") },
    AdapterEntry { langs: &["ruby"], exts: &[".rb"], grammar: Some("ruby") },
    AdapterEntry { langs: &["kotlin"], exts: &[".kt", ".kts"], grammar: Some("kotlin") },
    AdapterEntry { langs: &["swift"], exts: &[".swift"], grammar: Some("swift") },
    AdapterEntry { langs: &["scala"], exts: &[".scala"], grammar: Some("scala") },
    AdapterEntry { langs: &["lua"], exts: &[".lua"], grammar: Some("lua") },
    AdapterEntry { langs: &["elixir"], exts: &[".ex", ".exs"], grammar: Some("elixir") },
    AdapterEntry { langs: &["bash"], exts: &[".sh", ".bash"], grammar: Some("bash") },
];

fn adapter_for(path: &str) -> Option<&'static AdapterEntry> {
    let ext = extname(path);
    ADAPTERS.iter().find(|a| a.exts.contains(&ext.as_str()))
}

/// adapterFor(path) — class:"transitive": see module doc (drops the unportable `parse` closure).
#[allow(non_snake_case)]
pub fn adapterFor(path: &Value) -> Value {
    let p = str_qq(Some(path));
    match adapter_for(&p) {
        Some(a) => json!({ "langs": a.langs, "exts": a.exts }),
        None => Value::Null,
    }
}

/// supportedExtensions() — no-db helper, 0 args. Flattens every adapter's `exts`, in ADAPTERS order.
#[allow(non_snake_case)]
pub fn supportedExtensions() -> Value {
    Value::Array(ADAPTERS.iter().flat_map(|a| a.exts.iter().map(|e| json!(*e))).collect())
}

/// The grammars that have a WALK PROFILE — i.e. the ones that actually yield defs/imports/calls.
/// `None` is the oxc/JS lane; the rest are parse-treesitter's own walkers plus everything in
/// lang-profiles.rs. An entry in ADAPTERS only means "we recognise this extension", which is a
/// weaker claim — this is the list that says "and we extract from it". Since 2026-09-11 the two
/// coincide; the distinction is kept so the next grammar added to ADAPTERS without a profile is
/// still scoped out of analysis rather than scored from an empty parse.
fn walkable(grammar: Option<&str>) -> bool {
    match grammar {
        None | Some("typescript") | Some("tsx") | Some("python") | Some("rust") => true,
        Some(g) => crate::codebase__helpers_lang_profiles::PROFILED_GRAMMARS.contains(&g),
    }
}

/// The extensions the atlas can actually EXTRACT from, derived from the adapter table rather than
/// re-listed by hand.
///
/// WHY THIS EXISTS: four separate skills (constitution-judge / -estimate / -mine, ultra-run) each
/// carried their own private `code_re()` of `(mjs|cjs|js|jsx|ts|tsx|py)`. Rust was never added to
/// any of them, so the review pass silently walked past every .rs file — on a 46-file Rust project
/// the "deterministic" file review scored the two JavaScript tools in the repo and nothing else,
/// and antiIDE told the operator "no review snapshot for this file yet" for source it had fully
/// indexed. A hand-maintained copy of a capability list drifts the moment the capability changes;
/// deriving it means adding a walk profile is enough to make every analysis see the language.
///
/// Note this is deliberately NARROWER than `supportedExtensions()`: scoping analysis by ADAPTERS
/// would pull in Go/Java/C# files that can only ever score empty, which trades one wrong answer
/// (invisible) for another (a health score computed from nothing).
pub fn analyzable_extensions() -> Vec<&'static str> {
    ADAPTERS
        .iter()
        .filter(|a| walkable(a.grammar))
        .flat_map(|a| a.exts.iter().copied())
        .collect()
}

/// `analyzable_extensions()` as an anchored, case-insensitive filename regex — the shape every
/// consumer wanted, built once so the four of them cannot drift apart again.
pub fn analyzable_ext_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let alts: Vec<String> = analyzable_extensions()
            .iter()
            .map(|e| regex::escape(e.trim_start_matches('.')))
            .collect();
        Regex::new(&format!(r"(?i)\.({})$", alts.join("|"))).expect("built from a fixed adapter table")
    })
}

/// astParse(input, context) — default export, standard lane. See module doc for the two
/// documented divergences (PARITY-DEFERRED out-of-scope grammars; the dead JSX-fallback branch).
#[allow(non_snake_case)]
pub fn astParse(input: &Value, _db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let raw_path = match get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => return Err("ast-parse: path is required".into()),
    };
    // Portable graph: store POSIX separators (matches the JS source's own comment) — fs still
    // reads the OS-native rawPath below; only the stored/returned path is normalized.
    let path = raw_path.replace('\\', "/");

    let Some(adapter) = adapter_for(&path) else {
        let have: Vec<&str> = ADAPTERS.iter().flat_map(|a| a.exts.iter().copied()).collect();
        let ext = extname(&path);
        let ext_display = if ext.is_empty() { "(none)".to_string() } else { ext };
        return Err(format!(
            "ast-parse: language adapter (have: {}) for extension '{}' not found",
            have.join(", "),
            ext_display
        ));
    };

    let source = match nz(get("source")) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => str_qq(Some(other)),
        None => std::fs::read_to_string(&raw_path).map_err(|e| format!("ast-parse: {e}"))?,
    };

    // 2026-10-01: a Flow-typed file used to be skipped by the callers ("the JS/TS parser has no Flow
    // support") — 1160 files of the React repo with no symbols and no import edges. The pragma (or a
    // file that only LOOKS Flow-typed: `import type`, `declare`, `opaque type`) now routes through the
    // Flow pre-pass; a file oxc cannot read after it is an `Err`, never an empty "ok" parse.
    let flow = adapter.grammar.is_none() && (crate::codebase_atlas_ingest::isFlow(&path, &source) || crate::codebase_atlas_ingest::looksFlowTyped(&source));
    let parsed = match adapter.grammar {
        None if flow => crate::codebase_parse_js::parseJsFlow(&source, &path)?,
        None => crate::codebase_parse_js::parseJsChecked(&source, &path)?,
        Some(g) => crate::codebase_parse_treesitter::parseTreeSitter(&json!(source), &json!(path), &json!(g)),
    };

    if parsed.is_null() {
        // A grammar with no walk profile — nothing honest to return. Every ADAPTERS grammar has
        // one today; this guards the next one added without. See parse-treesitter.rs "Scope".
        return Ok(Value::Null);
    }

    let nodes = parsed.get("nodes").cloned().unwrap_or(Value::Null);
    let imports = parsed.get("imports").cloned().unwrap_or(Value::Null);
    let calls = nz(parsed.get("calls")).cloned().unwrap_or_else(|| json!([]));
    let refs = nz(parsed.get("refs")).cloned().unwrap_or_else(|| json!([]));
    let has_error = truthy(parsed.get("has_error"));

    let mut out = json!({
        "path": path,
        "lang": adapter.langs[0],
        "nodes": nodes,
        "imports": imports,
        "calls": calls,
        "refs": refs,
        "has_error": has_error,
    });
    if flow {
        // the health row says WHY it is degraded: how many errors, and the first one
        out["flow"] = json!(true);
        out["error_count"] = parsed.get("error_count").cloned().unwrap_or(json!(0));
        out["first_error"] = parsed.get("first_error").cloned().unwrap_or(Value::Null);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn analyzable_extensions_track_the_walk_profiles_not_a_hand_list() {
        let exts = analyzable_extensions();
        // Every language with a walk profile must be analysable. `.rs` is the regression: four
        // skills each carried a private `(mjs|cjs|js|jsx|ts|tsx|py)` and none gained Rust, so the
        // review pass scored 2 JavaScript files in a 46-file Rust repo and called it a review.
        for want in [".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".py", ".rs"] {
            assert!(exts.contains(&want), "{want} has a walk profile — analysis must see it: {exts:?}");
        }
        // 2026-09-11: every ADAPTERS grammar has a walk profile, so the analysable set IS the
        // adapter set. The filter stays: a grammar added to ADAPTERS without a profile must drop
        // out here rather than be scored from an empty parse.
        for now_wired in [".go", ".java", ".rb", ".cs", ".php", ".c", ".cpp", ".kt", ".swift", ".scala", ".lua", ".ex", ".sh"] {
            assert!(exts.contains(&now_wired), "{now_wired} has a walk profile — analysis must see it: {exts:?}");
        }
        assert_eq!(exts.len(), supportedExtensions().as_array().unwrap().len(), "adapter set and analysable set coincide today");
        let re = analyzable_ext_regex();
        assert!(re.is_match("crates/core/src/youtube.rs"), "the exact file the operator reported");
        assert!(re.is_match("src/App.TSX"), "case-insensitive");
        assert!(!re.is_match("README.md"));
        assert!(re.is_match("main.go"));
    }

    #[test]
    fn supported_extensions_flattens_all_adapters_in_order() {
        let exts = supportedExtensions();
        let arr = exts.as_array().unwrap();
        assert_eq!(arr[0], json!(".js"));
        assert_eq!(arr[1], json!(".mjs"));
        assert_eq!(arr[2], json!(".cjs"));
        assert_eq!(arr[3], json!(".ts"));
        assert!(arr.contains(&json!(".py")));
        assert!(arr.contains(&json!(".sh")));
        assert!(arr.contains(&json!(".bash")));
        assert_eq!(arr.last().unwrap(), &json!(".bash"));
    }

    #[test]
    fn adapter_for_matches_js_lang_and_drops_the_parse_closure() {
        let v = adapterFor(&json!("src/foo.JS")); // uppercase extension, still matches
        assert_eq!(v, json!({ "langs": ["javascript"], "exts": [".js", ".mjs", ".cjs"] }));
        assert_eq!(v.as_object().unwrap().len(), 2); // no "parse" key
    }

    #[test]
    fn adapter_for_jsx_routes_to_tsx_grammar_entry() {
        let v = adapterFor(&json!("widget.jsx"));
        assert_eq!(v["langs"], json!(["jsx"]));
    }

    #[test]
    fn adapter_for_unknown_extension_is_null() {
        assert_eq!(adapterFor(&json!("no-ext-file")), Value::Null);
        assert_eq!(adapterFor(&json!("weird.xyz")), Value::Null);
    }

    #[test]
    fn ast_parse_js_function_with_call_and_decision() {
        let source = "function add(a, b) {\n  if (a > 0) {\n    return helper(a, b);\n  }\n  return b;\n}\n";
        let r = astParse(&json!({ "path": "foo.js", "source": source }), &db()).unwrap();
        assert_eq!(r["path"], json!("foo.js"));
        assert_eq!(r["lang"], json!("javascript"));
        assert_eq!(r["has_error"], json!(false));
        let nodes = r["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1]["name"], json!("add"));
        assert_eq!(nodes[1]["span"]["complexity"], json!(2));
        let calls = r["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["callee"], json!("helper"));
        assert!(r["imports"].as_array().unwrap().is_empty());
    }

    #[test]
    fn ast_parse_normalizes_backslashes_in_the_returned_path_and_node_ids() {
        let r = astParse(&json!({ "path": "src\\utils.js", "source": "const z = 1;\n" }), &db()).unwrap();
        assert_eq!(r["path"], json!("src/utils.js"));
        assert_eq!(r["nodes"][1]["id"], json!("src/utils.js#z"));
    }

    #[test]
    fn ast_parse_typescript_extension_uses_typescript_grammar() {
        let source = "function straight() {\n  return 1;\n}\n";
        let r = astParse(&json!({ "path": "bar.ts", "source": source }), &db()).unwrap();
        assert_eq!(r["lang"], json!("typescript"));
        assert_eq!(r["nodes"][1]["name"], json!("straight"));
        assert_eq!(r["has_error"], json!(false));
    }

    #[test]
    fn ast_parse_tsx_extension_uses_tsx_grammar() {
        let source = "export function Comp() {\n  return call();\n}\n";
        let r = astParse(&json!({ "path": "comp.tsx", "source": source }), &db()).unwrap();
        assert_eq!(r["lang"], json!("tsx"));
        assert_eq!(r["nodes"][1]["name"], json!("Comp"));
        assert_eq!(r["nodes"][1]["exported"], json!(true));
    }

    #[test]
    fn ast_parse_jsx_extension_reports_jsx_lang_via_tsx_grammar() {
        let source = "export function Widget() {\n  return draw();\n}\n";
        let r = astParse(&json!({ "path": "widget.jsx", "source": source }), &db()).unwrap();
        assert_eq!(r["lang"], json!("jsx"));
        assert_eq!(r["nodes"][1]["name"], json!("Widget"));
    }

    #[test]
    fn ast_parse_python_extension_uses_python_grammar() {
        let source = "def compute(x):\n    if x > 0:\n        return helper(x)\n    return x\n";
        let r = astParse(&json!({ "path": "mod.py", "source": source }), &db()).unwrap();
        assert_eq!(r["lang"], json!("python"));
        assert_eq!(r["nodes"][1]["name"], json!("compute"));
        assert_eq!(r["nodes"][1]["span"]["complexity"], json!(2));
        let calls = r["calls"].as_array().unwrap();
        assert_eq!(calls[0]["callee"], json!("helper"));
    }

    #[test]
    fn ast_parse_walks_every_adapter_grammar() {
        // Once a documented divergence (Go returned Null); since 2026-09-11 every ADAPTERS grammar
        // has a walk profile, so the dispatcher hands back a real extract with the adapter's lang.
        let r = astParse(&json!({ "path": "main.go", "source": "package main\n\nfunc Run() {}\n" }), &db()).unwrap();
        assert_eq!(r["lang"], json!("go"));
        assert!(r["nodes"].as_array().unwrap().iter().any(|n| n["name"] == json!("Run")));
        for (path, source) in [("a.kt", "fun f() = 1\n"), ("a.swift", "func f() {}\n"), ("a.ex", "defmodule A do\nend\n"), ("a.sh", "f() { :; }\n")] {
            let r = astParse(&json!({ "path": path, "source": source }), &db()).unwrap();
            assert!(!r.is_null(), "{path} must extract");
        }
    }

    #[test]
    fn ast_parse_missing_path_errors() {
        assert!(astParse(&json!({}), &db()).unwrap_err().contains("path is required"));
    }

    #[test]
    fn ast_parse_unknown_extension_errors() {
        let e = astParse(&json!({ "path": "readme.xyz", "source": "hi" }), &db()).unwrap_err();
        assert!(e.contains("not found"));
    }

    // ── Flow (2026-10-01): the React repo's 1160 `@flow` files used to be skipped, with no symbols ──

    fn fx_flow(rel: &str) -> String {
        std::fs::read_to_string(format!("{}/fixtures/fx-flow/{rel}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }
    fn names(r: &Value) -> Vec<String> {
        r["nodes"].as_array().unwrap().iter().filter(|n| n["type"] != json!("file")).map(|n| n["name"].as_str().unwrap_or("").to_string()).collect()
    }

    #[test]
    fn a_real_react_flow_file_gets_its_symbols_and_imports() {
        let path = "packages/shared/getComponentNameFromType.js";
        let r = astParse(&json!({ "path": path, "source": fx_flow(path) }), &db()).unwrap();
        assert_eq!(r["has_error"], json!(false), "{r}");
        assert!(names(&r).contains(&"getComponentNameFromType".to_string()), "{:?}", names(&r));
        let specs: Vec<&str> = r["imports"].as_array().unwrap().iter().map(|i| i["specifier"].as_str().unwrap()).collect();
        assert!(specs.contains(&"shared/ReactSymbols") && specs.contains(&"shared/ReactTypes"), "{specs:?}");
        let kinds: Vec<&str> = r["imports"].as_array().unwrap().iter().filter(|i| i["specifier"] == json!("shared/ReactTypes")).map(|i| i["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["type"], "`import type` is a type edge");
        let f = r["nodes"].as_array().unwrap().iter().find(|n| n["name"] == json!("getComponentNameFromType")).unwrap();
        assert_eq!(f["exported"], json!(true));
    }

    #[test]
    fn a_big_react_flow_file_keeps_its_line_numbers() {
        // ReactChildFiber.js: 2000+ lines of casts, maybe types and generics — a rewrite that moved one newline would shift every symbol below it
        let path = "packages/react-reconciler/src/ReactChildFiber.js";
        let src = fx_flow(path);
        let r = astParse(&json!({ "path": path, "source": src.clone() }), &db()).unwrap();
        assert_eq!(r["has_error"], json!(false), "{r}");
        assert!(names(&r).len() >= 15, "{:?}", names(&r));
        let at = |name: &str| -> i64 { r["nodes"].as_array().unwrap().iter().find(|n| n["name"] == json!(name)).unwrap()["span"]["start_line"].as_i64().unwrap() };
        for name in ["createChildReconciler", "reconcileChildFibers", "mountChildFibers"] {
            if r["nodes"].as_array().unwrap().iter().any(|n| n["name"] == json!(name)) {
                let line = src.lines().nth(at(name) as usize - 1).unwrap();
                assert!(line.contains(name), "{name} reported at line {} which reads `{line}`", at(name));
            }
        }
        let create = at("createChildReconciler");
        assert!(src.lines().nth(create as usize - 1).unwrap().contains("function createChildReconciler"));
    }

    #[test]
    fn flow_without_a_pragma_is_recognised_by_its_syntax() {
        let r = astParse(&json!({ "path": "lib.js", "source": "import type {A} from './a';\nexport function f(x: ?A): A { return (x: any); }\n" }), &db()).unwrap();
        assert_eq!(r["has_error"], json!(false), "{r}");
        assert_eq!(names(&r), vec!["f"]);
        assert_eq!(r["flow"], json!(true));
    }

    #[test]
    fn plain_javascript_is_not_run_through_the_flow_pass() {
        let r = astParse(&json!({ "path": "a.js", "source": "export function f(x) { return x ? 1 : 2; }\n" }), &db()).unwrap();
        assert_eq!(r.get("flow"), None);
    }

    #[test]
    fn a_flow_construct_the_pass_does_not_know_is_degraded_and_says_where() {
        // a `renders` type (Flow component syntax) the strip pass does not know: the row says what and where (`interface extends` was this fixture until the strip learned it)
        let src = "// @flow\nexport function f() {}\ntype P = renders Foo;\nexport function g() {}\n";
        let r = astParse(&json!({ "path": "p.js", "source": src }), &db());
        match r {
            Ok(v) => {
                assert_eq!(v["has_error"], json!(true));
                assert!(v["first_error"].as_str().unwrap().contains("line 3"), "{v}");
            }
            Err(e) => assert!(e.starts_with("flow:") && e.contains("line 3"), "{e}"),
        }
    }
}
