//! ts-query.rs — twin of skills/constitution/_helpers/ts-query.mjs, the language-agnostic
//! tree-sitter QUERY engine. NAMES ARE CONTRACT: `grammarForPath` (no-db helper, fixturable),
//! `getTree` / `queryMatches` (TRANSITIVE — real native code + `#[cfg(test)]` coverage, NO JSON
//! fixture: a parsed `Tree`/`Language` handle is not a `serde_json::Value`, same convention as
//! reasoning-templates.rs's fill/rng/makePicker).
//!
//! ## Architecture decision (this pass): native tree-sitter, not WASM
//! JS parses via `web-tree-sitter` (WASM bindings) + `tree-sitter-wasms` (bundled `.wasm` grammar
//! files) because Node has no zero-friction native tree-sitter binding story across platforms.
//! Rust has the OPPOSITE: `tree-sitter` (the official Rust binding to the same C library, no WASM
//! runtime) plus per-language grammar crates on crates.io that statically link the grammar into
//! the binary — no file-read, no async init step. JS's `getLanguage()` is async because it awaits
//! `Parser.init()` (loads the WASM runtime once) and `readFileSync` + `Language.load()` (reads a
//! `.wasm` grammar file) — both are I/O. The native side has NO equivalent I/O: the grammar is a
//! linked-in `LanguageFn`. So `getTree` collapsing from `async` (JS) to a plain sync `pub fn`
//! (Rust) is a legitimate, documented SIMPLIFICATION, not a fidelity gap — there is nothing left
//! to await.
//!
//! ## Scope: every grammar in `GRAMMAR_BY_EXT` parses
//! Originally only `tsx` / `typescript` / `javascript` / `python` were wired (the JS lane's
//! parity scope), then `rust` (2026-08, because the engine itself is Rust). On 2026-09-11 the
//! remaining thirteen — go, java, c_sharp, c, cpp, php, ruby, kotlin, swift, scala, lua, elixir,
//! bash — joined, closing the gap where an extension mapped to a grammar name that `getTree`
//! then refused. The constitution's seed laws still supply `ast` queries only for tsx /
//! typescript / python; a law with no query for a file's grammar simply does not fire for it
//! (`Query::new` against the wrong grammar fails → `queryMatches` returns `None`), and
//! cognitive.rs's `FAMILY` table decides which grammars get statement-level metrics. Parsing
//! is now uniformly available; which analyses use it is each consumer's own table.
//!
//! `getTree` / `queryMatches` return `None` for any grammar name outside that table — mirroring,
//! not violating, the JS module's own "fails soft" contract (`getTree` and `queryMatches` in the
//! JS source NEVER throw; an unloadable/unsupported grammar or a malformed query already returns
//! `null` there, and the caller already treats `null` as "this law doesn't fire for this file" —
//! see detectors.mjs's `ast` detector: `if (hits == null) return null;`). `javascript` is
//! accepted defensively even though `grammarForPath` in THIS codebase never actually produces
//! that string (JS-family extensions route through `'tsx'`, a superset grammar — see
//! `GRAMMAR_BY_EXT` below).
//!
//! ## Necessary Rust-only API surface difference: `queryMatches` takes an explicit `source`
//! JS's `Node.text` getter returns the node's substring directly — `web-tree-sitter` keeps a
//! reference to the parsed source string internally. Native `tree-sitter`'s `Tree`/`Node` are
//! zero-copy: they store byte OFFSETS into the buffer you parsed, not the buffer itself, so
//! extracting text requires the ORIGINAL bytes at call time (`Node::utf8_text(source_bytes)`).
//! This twin's `queryMatches` therefore takes one extra parameter, `source: &str`, versus the JS
//! signature `queryMatches(grammar, language, tree, queryStr, filter)` — an unavoidable
//! consequence of the zero-copy design, not a behavior divergence (callers already have the
//! source string on hand, since they just parsed it via `getTree`).
//!
//! ## Verified vs assumed — read before trusting this module at exotic edges
//! - `grammarForPath`: pure string logic, port verified against the JS source by inspection +
//!   the `#[cfg(test)]` cases below (no build needed to reason about it).
//! - `queryMatches` QUERY STRINGS: tree-sitter's query language (S-expressions) is defined by the
//!   C library and is IDENTICAL between native and WASM bindings (same parser, same query
//!   engine) — this is a structural fact, not an assumption. The native tests below run the
//!   ACTUAL queries this codebase ships in seed-laws.mjs's `SEED_LAWS` (`detector:'ast'` entries:
//!   empty `catch {}` / bare Python `except: pass`, loose `==`/`!=`, `var`, `debugger_statement`)
//!   against representative JS/TS/Python snippets and assert on captured line/text — real
//!   end-to-end coverage of the part that matters (what this codebase's laws actually query).
//! - Node-type / field-name PARITY between the native crates pinned here
//!   (tree-sitter-javascript 0.25.0, tree-sitter-typescript 0.23.2, tree-sitter-python 0.25.0,
//!   tree-sitter core 0.26.11) and whatever grammar commit `tree-sitter-wasms` bundles for
//!   web-tree-sitter: CROSS-CHECKED against a live Node run of the real JS `ts-query.mjs` in this
//!   pass (dynamic `import()`, same grammar names, same query strings, same source snippets as
//!   the native tests below). Every case matched byte-for-byte: `grammarForPath` for
//!   `.mjs/.jsx/.ts/.py/.go/Makefile`; empty-catch / non-empty-catch (`{}` / 0 hits); loose-`==`
//!   (`"a == b"` at line 1, `===` correctly excluded); TypeScript `var` vs `let` (1 hit, `let` is
//!   a distinct `lexical_declaration` node in both bindings — confirms node-type naming parity,
//!   not just presence) and `debugger_statement` at line 3; Python bare `except: pass` (1 hit,
//!   `"pass"` at line 4) vs `except Exception as e: log(e)` (0 hits); and the 6-hit cap. This
//!   raises node-type/field-name parity from "high confidence by same-upstream-repo inference" to
//!   "confirmed for every node type and query this codebase's real laws exercise" — still not an
//!   exhaustive claim over exotic/rare node shapes this pass didn't construct a case for.
//! - `cargo add tree-sitter tree-sitter-javascript tree-sitter-typescript tree-sitter-python`
//!   resolved cleanly against crates.io (0.26.11 / 0.25.0 / 0.23.2 / 0.25.0, pulling in
//!   `streaming-iterator` transitively — tree-sitter 0.22+'s `QueryCursor::captures` returns a
//!   `StreamingIterator`, not a plain `Iterator`, which this module's code accounts for).
//!   `cargo check --lib` and `cargo test --lib constitution__helpers_ts_query` were BOTH run this
//!   pass (cold-cache C-grammar compile ~2 min for check, ~5 min for the full lib test build) —
//!   clean compile, all 13 native tests pass. This is a verified module, not an unbuilt one.
use serde_json::{json, Value};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Language, Parser, Query, QueryCursor, Tree};

fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

// extension → grammar name. FULL table, verbatim from GRAMMAR_BY_EXT (JS-family routes through
// 'tsx', a superset that parses plain JS + JSX cleanly, so one grammar name covers
// .js/.mjs/.cjs/.jsx/.tsx). Kept complete even for grammars this twin doesn't parse (module doc
// scoping) — the mapping itself is pure/free.
const GRAMMAR_BY_EXT: &[(&str, &str)] = &[
    (".tsx", "tsx"),
    (".jsx", "tsx"),
    (".js", "tsx"),
    (".mjs", "tsx"),
    (".cjs", "tsx"),
    (".ts", "typescript"),
    (".mts", "typescript"),
    (".cts", "typescript"),
    (".py", "python"),
    (".go", "go"),
    (".java", "java"),
    (".rb", "ruby"),
    (".rs", "rust"),
    (".php", "php"),
    (".c", "c"),
    (".h", "c"),
    (".cpp", "cpp"),
    (".cc", "cpp"),
    (".cs", "c_sharp"),
    // 2026-09-11: the ADAPTERS extensions (ast-parse.rs) this table was missing. Without them a
    // Kotlin or Swift file mapped to no grammar at all, so the constitution never saw it.
    (".cxx", "cpp"),
    (".hpp", "cpp"),
    (".hh", "cpp"),
    (".kt", "kotlin"),
    (".kts", "kotlin"),
    (".swift", "swift"),
    (".scala", "scala"),
    (".lua", "lua"),
    (".ex", "elixir"),
    (".exs", "elixir"),
    (".sh", "bash"),
    (".bash", "bash"),
];

pub fn grammarForPath(path: &Value) -> Value {
    let p = js_string(path).to_lowercase();
    match p.rfind('.') {
        None => Value::Null,
        Some(dot) => match GRAMMAR_BY_EXT.iter().find(|(ext, _)| *ext == &p[dot..]) {
            Some((_, grammar)) => json!(*grammar),
            None => Value::Null,
        },
    }
}

// grammar name → linked-in grammar. See module doc "Scope" section.
fn language_for_grammar(grammar: &str) -> Option<Language> {
    match grammar {
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "javascript" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        // Rust is wired because the substrate's own engine is Rust: an atlas blind to the codebase
        // it runs on reports a clean, empty map — the same silent-degradation shape the scan's
        // dead dimensions had. The others below remain unwired for lack of a grammar crate, not
        // for parity: the JS lane this once deferred to no longer exists.
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        // 2026-09-11: the rest of the ADAPTERS table. Every one of these was a recognised
        // extension that parsed to nothing — see Cargo.toml's note on the grammar crates.
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "c_sharp" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        "c" => Some(tree_sitter_c::LANGUAGE.into()),
        "cpp" => Some(tree_sitter_cpp::LANGUAGE.into()),
        // LANGUAGE_PHP (not _ONLY): real files open with `<?php`, and the ONLY variant rejects
        // the tag.
        "php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),
        "ruby" => Some(tree_sitter_ruby::LANGUAGE.into()),
        "kotlin" => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
        "swift" => Some(tree_sitter_swift::LANGUAGE.into()),
        "scala" => Some(tree_sitter_scala::LANGUAGE.into()),
        "lua" => Some(tree_sitter_lua::LANGUAGE.into()),
        "elixir" => Some(tree_sitter_elixir::LANGUAGE.into()),
        "bash" => Some(tree_sitter_bash::LANGUAGE.into()),
        _ => None,
    }
}

/// getTree(grammar, source) — JS is `async` (awaits WASM init + a grammar file read); the native
/// side has no I/O step to await (see module doc), so this is a plain sync fn. `None` covers both
/// "unsupported grammar" and "language failed to attach to a fresh Parser" — JS's `getTree`
/// returns `null` for the same two situations (an unloadable grammar, or `parser.setLanguage`
/// throwing), never an exception.
pub fn getTree(grammar: &str, source: &str) -> Option<(Tree, Language)> {
    let language = language_for_grammar(grammar)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(source, None)?;
    Some((tree, language))
}

const HITS_CAP: usize = 6;

/// queryMatches(grammar, language, tree, source, queryStr, filter) — see module doc for why
/// `source` is a necessary extra parameter versus the JS signature. `filter == Some("empty")`
/// keeps only captures whose node has zero named children (mirrors JS `filter === 'empty'`).
pub fn queryMatches(
    grammar: &str,
    language: &Language,
    tree: &Tree,
    source: &str,
    query_str: &str,
    filter: Option<&str>,
) -> Option<Vec<Value>> {
    queryMatchesUpTo(grammar, language, tree, source, query_str, filter, HITS_CAP)
}

/// `queryMatches` with the limit chosen by the caller: the detectors ask for exactly the cap they list (5 by default), or `usize::MAX` when a context is `uncapped` -
/// a cap limits what is listed, never what is counted (2026-10-06, see `detectors::cap_of`).
pub fn queryMatchesUpTo(
    _grammar: &str,
    language: &Language,
    tree: &Tree,
    source: &str,
    query_str: &str,
    filter: Option<&str>,
    cap: usize,
) -> Option<Vec<Value>> {
    let query = Query::new(language, query_str).ok()?; // malformed query for this grammar → None, law doesn't fire
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source.as_bytes());
    let mut out: Vec<Value> = Vec::new();
    while let Some((m, capture_index)) = captures.next() {
        if out.len() >= cap {
            break;
        }
        let node = m.captures[*capture_index].node;
        if filter == Some("empty") && node.named_child_count() != 0 {
            continue;
        }
        // `allow-null`: `x == null` / `undefined != x` is the idiomatic null-OR-undefined check
        // (ESLint `eqeqeq: [..., 'allow-null']`); a loose-equality law with this filter keeps
        // every other `==` and lets that one idiom through.
        if filter == Some("allow-null") && node.kind() == "binary_expression" {
            let is_nullish = |side: Option<Node>| side.map_or(false, |n| matches!(n.kind(), "null" | "undefined"));
            if is_nullish(node.child_by_field_name("left")) || is_nullish(node.child_by_field_name("right")) {
                continue;
            }
        }
        let line = node.start_position().row as i64 + 1;
        let full_text = node.utf8_text(source.as_bytes()).unwrap_or("");
        let first_line = full_text.split('\n').next().unwrap_or("");
        let text: String = first_line.chars().take(160).collect();
        out.push(json!({ "line": line, "text": text }));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── grammarForPath ──────────────────────────────────────────────────────────────────────
    #[test]
    fn grammar_for_path_maps_js_family_to_tsx() {
        assert_eq!(grammarForPath(&json!("skills/foo.mjs")), json!("tsx"));
        assert_eq!(grammarForPath(&json!("Foo.JSX")), json!("tsx")); // lowercased first
        assert_eq!(grammarForPath(&json!("a/b/c.cjs")), json!("tsx"));
    }

    #[test]
    fn grammar_for_path_maps_ts_and_py() {
        assert_eq!(grammarForPath(&json!("x.ts")), json!("typescript"));
        assert_eq!(grammarForPath(&json!("x.mts")), json!("typescript"));
        assert_eq!(grammarForPath(&json!("x.py")), json!("python"));
    }

    #[test]
    fn grammar_for_path_maps_deferred_but_present_extensions() {
        assert_eq!(grammarForPath(&json!("main.go")), json!("go"));
        assert_eq!(grammarForPath(&json!("Main.java")), json!("java"));
        assert_eq!(grammarForPath(&json!("lib.rs")), json!("rust"));
        assert_eq!(grammarForPath(&json!("prog.c")), json!("c"));
        assert_eq!(grammarForPath(&json!("prog.h")), json!("c"));
        assert_eq!(grammarForPath(&json!("prog.cpp")), json!("cpp"));
    }

    #[test]
    fn grammar_for_path_no_dot_or_unknown_ext_is_null() {
        assert_eq!(grammarForPath(&json!("Makefile")), Value::Null);
        assert_eq!(grammarForPath(&json!("readme.md")), Value::Null);
        assert_eq!(grammarForPath(&json!(123)), Value::Null); // "123" has no '.'
    }

    // ── getTree / queryMatches — real native parses + the actual queries this codebase ships ──
    #[test]
    fn empty_catch_clause_matches_via_empty_filter() {
        let src = "function f() { try { g(); } catch (e) {} }";
        let (tree, lang) = getTree("tsx", src).expect("tsx grammar should load");
        let hits = queryMatches(
            "tsx",
            &lang,
            &tree,
            src,
            "(catch_clause body: (statement_block) @hit)",
            Some("empty"),
        )
        .expect("well-formed query");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["text"], json!("{}"));
    }

    #[test]
    fn non_empty_catch_clause_is_excluded_by_empty_filter() {
        let src = "function f() { try { g(); } catch (e) { log(e); } }";
        let (tree, lang) = getTree("tsx", src).unwrap();
        let hits = queryMatches(
            "tsx",
            &lang,
            &tree,
            src,
            "(catch_clause body: (statement_block) @hit)",
            Some("empty"),
        )
        .unwrap();
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn loose_equality_query_finds_double_equals_not_triple() {
        let src = "if (a == b) { x = 1; } if (c === d) { x = 2; }";
        let (tree, lang) = getTree("tsx", src).unwrap();
        let hits = queryMatches(
            "tsx",
            &lang,
            &tree,
            src,
            r#"(binary_expression operator: ["==" "!="]) @hit"#,
            None,
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["text"], json!("a == b"));
        assert_eq!(hits[0]["line"], json!(1));
    }

    #[test]
    fn no_var_query_and_debugger_query_on_typescript_grammar() {
        let src = "var x = 1;\nlet y = 2;\ndebugger;\n";
        let (tree, lang) = getTree("typescript", src).unwrap();
        let vars = queryMatches("typescript", &lang, &tree, src, "(variable_declaration) @hit", None).unwrap();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0]["line"], json!(1));
        let dbg = queryMatches("typescript", &lang, &tree, src, "(debugger_statement) @hit", None).unwrap();
        assert_eq!(dbg.len(), 1);
        assert_eq!(dbg[0]["line"], json!(3));
    }

    #[test]
    fn the_no_var_seed_law_sees_every_form_of_var_including_the_loop_head() {
        // found by the law-check A/B: `for (var o of orders)` is a for_in_statement, not a variable_declaration, so the check counted 2 of 3 `var`s in an agent's file
        let seed = crate::constitution__helpers_seed_laws::SEED_LAWS.as_array().unwrap().iter().find(|l| l["key"] == "no-var").cloned().expect("the seed law");
        let query = seed["config_json"]["queries"]["typescript"].as_str().unwrap().to_string();
        let src = "var a = 1;\nfor (var i = 0; i < 3; i++) {}\nfor (var k in o) {}\nfor (var v of xs) {}\n";
        let (tree, lang) = getTree("typescript", src).unwrap();
        let hits = queryMatches("typescript", &lang, &tree, src, &query, None).unwrap();
        assert_eq!(hits.iter().map(|h| h["line"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 2, 3, 4], "{hits:?}");
        let clean = "let a = 1;\nconst b = 2;\nfor (let i = 0; i < 3; i++) {}\nfor (const k in o) {}\nfor (const v of xs) {}\nconst s = \"var x\"; // var y\n";
        let (tree2, lang2) = getTree("typescript", clean).unwrap();
        assert!(queryMatches("typescript", &lang2, &tree2, clean, &query, None).unwrap().is_empty(), "let/const loops and the word in a string or comment are not `var`");
    }

    #[test]
    fn python_bare_except_pass_matches_the_seed_law_query() {
        let src = "try:\n    f()\nexcept:\n    pass\n";
        let (tree, lang) = getTree("python", src).expect("python grammar should load");
        let hits = queryMatches(
            "python",
            &lang,
            &tree,
            src,
            "(except_clause (block (pass_statement)) @hit)",
            None,
        )
        .expect("well-formed query");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["text"], json!("pass"));
        assert_eq!(hits[0]["line"], json!(4));
    }

    #[test]
    fn python_except_with_real_handling_does_not_match() {
        let src = "try:\n    f()\nexcept Exception as e:\n    log(e)\n";
        let (tree, lang) = getTree("python", src).unwrap();
        let hits = queryMatches(
            "python",
            &lang,
            &tree,
            src,
            "(except_clause (block (pass_statement)) @hit)",
            None,
        )
        .unwrap();
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn unsupported_grammar_returns_none_never_panics() {
        assert!(getTree("nonsense-grammar", "whatever").is_none());
    }

    #[test]
    fn every_grammar_in_the_extension_table_parses() {
        // The gap this closes: an extension that mapped to a grammar name getTree then refused.
        let smoke: &[(&str, &str)] = &[
            ("go", "package main"), ("java", "class A {}"), ("c_sharp", "class A {}"), ("c", "int main(void) { return 0; }"),
            ("cpp", "int main() { return 0; }"), ("php", "<?php\nfunction f() {}"), ("ruby", "puts 1"), ("kotlin", "fun f() = 1\n"),
            ("swift", "func f() {}"), ("scala", "object A"), ("lua", "local x = 1"), ("elixir", "defmodule A do\nend"), ("bash", "echo hi"),
        ];
        for (g, src) in smoke {
            let (tree, _) = getTree(g, src).unwrap_or_else(|| panic!("{g} should load"));
            assert!(!tree.root_node().has_error(), "{g}: {}", tree.root_node().to_sexp());
        }
        for (_, grammar) in GRAMMAR_BY_EXT {
            assert!(getTree(grammar, "").is_some(), "{grammar} is in GRAMMAR_BY_EXT but has no linked grammar");
        }
    }

    #[test]
    fn allow_null_filter_keeps_loose_equality_but_lets_the_null_idiom_through() {
        let src = "if (a == null) {} if (undefined != b) {} if (c == d) {} if (e != 0) {}";
        let (tree, lang) = getTree("tsx", src).unwrap();
        let q = "(binary_expression operator: [\"==\" \"!=\"]) @hit";
        let all = queryMatches("tsx", &lang, &tree, src, q, None).unwrap();
        assert_eq!(all.len(), 4);
        let kept = queryMatches("tsx", &lang, &tree, src, q, Some("allow-null")).unwrap();
        let texts: Vec<&str> = kept.iter().map(|h| h["text"].as_str().unwrap()).collect();
        assert_eq!(texts, vec!["c == d", "e != 0"]);
    }

    #[test]
    fn malformed_query_returns_none_not_a_panic() {
        let (tree, lang) = getTree("tsx", "let x = 1;").unwrap();
        let hits = queryMatches("tsx", &lang, &tree, "let x = 1;", "(this is not @valid (query", None);
        assert!(hits.is_none());
    }

    #[test]
    fn hits_cap_stops_at_six() {
        let src = (0..10).map(|i| format!("if (a{i} == b{i}) {{}}\n")).collect::<String>();
        let (tree, lang) = getTree("tsx", &src).unwrap();
        let hits = queryMatches(
            "tsx",
            &lang,
            &tree,
            &src,
            r#"(binary_expression operator: ["==" "!="]) @hit"#,
            None,
        )
        .unwrap();
        assert_eq!(hits.len(), HITS_CAP);
    }
}
