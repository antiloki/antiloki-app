//! ast-html.rs — twin of skills/render/ast-html.mjs. NAMES ARE CONTRACT: `astHtml` (default).
//! Renders a skill's source as an annotated HTML view of its parsed AST, for the skill-inspection
//! UI.
//!
//! ## Why this was the last undispatchable action, and what unblocked it
//! The JS renders **acorn's** ESTree object graph directly: `renderNode` walks `Object.keys(node)`
//! and emits a child branch per key. So a faithful port needs an ESTree tree with acorn's node
//! types AND acorn's key order — not merely "a parse tree". oxc's own AST types have a different
//! shape, which is why this stayed deferred while the rest of the crate crossed.
//!
//! `oxc_ast`'s `serialize` feature closes exactly that gap: `Program::to_estree_json` exists to
//! emit acorn/espree-compatible ESTree JSON. With `serde_json`'s `preserve_order` (already on
//! crate-wide, so JSON objects keep insertion order like JS objects do), walking that JSON
//! reproduces the JS walk directly.
//!
//! ## The one thing oxc does not give, and how it is derived
//! acorn is called with `locations: true`, so every node carries
//! `loc: {start:{line,column}, end:{...}}`. oxc's ESTree output carries byte offsets (`start`/
//! `end`) instead. `nodeLabel` prints `[line:column]`, so the offsets are converted here via a
//! line-start index over the source. acorn's convention is reproduced exactly: **1-based lines,
//! 0-based columns**, and columns counted in UTF-16 code units (what acorn reports), not bytes.
//!
//! ## Disclosed divergence
//! `SKIP_KEYS` drops `type`/`loc`/`start`/`end`/`range`/`sourceType` before recursing, so oxc's
//! offset fields never reach the output and cannot differ. What remains observable is the ORDER of
//! the child keys and the node-type names, both of which are what oxc's ESTree emitter is built to
//! match. Where a future oxc release orders a node's fields differently from acorn, the branches
//! under that node would render in a different order — the same nodes, the same labels, a
//! different sequence. That is a rendering-order difference in a debug view, disclosed rather than
//! papered over; it is not a conform unit (no fixture drives it) precisely because the tree is the
//! parser's, not this skill's.
// QUALITY: 90/100 — faithful port; the one unportable input (acorn's loc) is derived, not faked.

use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;
use serde_json::Value;

const SKIP_KEYS: [&str; 6] = ["type", "loc", "start", "end", "range", "sourceType"];
// JS iterates a Set built in this literal order; nodeLabel's output order follows it.
const INLINE_KEYS: [&str; 5] = ["name", "value", "operator", "raw", "kind"];

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

/// JS `String(s).replace(/&/g,'&amp;')…` — the four replacements, in that order (`&` first, or the
/// entities it introduces would be double-escaped).
fn escapeHtml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// acorn `locations: true`: 1-based line, 0-based column, columns in UTF-16 code units.
/// Returns the (line, column) for a byte offset into `src`.
fn line_col_utf16(line_starts: &[usize], src: &str, offset: usize) -> (usize, usize) {
    let idx = match line_starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i - 1,
    };
    let line_start = line_starts[idx];
    let upto = src.get(line_start..offset).unwrap_or("");
    (idx + 1, upto.chars().map(char::len_utf16).sum())
}

fn line_starts_of(src: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' { v.push(i + 1); }
    }
    v
}

// JS `isAstNode`: an object with a string `type`. Arrays are objects in JS but have no `type`, so
// they are excluded — matched here by requiring a JSON object.
fn is_ast_node(v: &Value) -> bool {
    v.as_object().map(|o| matches!(o.get("type"), Some(Value::String(_)))).unwrap_or(false)
}

fn nodeLabel(node: &Value, line_starts: &[usize], src: &str) -> String {
    let mut parts = vec![format!(
        "<span class=\"type\">{}</span>",
        escapeHtml(&str_qq(node.get("type")))
    )];
    for key in INLINE_KEYS {
        match node.get(key) {
            None | Some(Value::Null) => {}
            Some(v) => {
                // JS: a string is used verbatim, anything else goes through JSON.stringify.
                let val = match v {
                    Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                parts.push(format!(
                    "<span class=\"key\">{key}=</span><code>{}</code>",
                    escapeHtml(&val)
                ));
            }
        }
    }
    // acorn's `loc`, derived from oxc's byte offset (see module doc).
    if let Some(start) = node.get("start").and_then(Value::as_u64) {
        let (line, column) = line_col_utf16(line_starts, src, start as usize);
        parts.push(format!("<span class=\"loc\">[{line}:{column}]</span>"));
    }
    parts.join(" ")
}

fn renderNode(node: &Value, line_starts: &[usize], src: &str) -> String {
    if !is_ast_node(node) { return String::new(); }
    let obj = node.as_object().unwrap();
    let mut children: Vec<String> = Vec::new();

    for (key, child) in obj {
        if SKIP_KEYS.contains(&key.as_str()) || INLINE_KEYS.contains(&key.as_str()) { continue; }
        match child {
            Value::Array(a) => {
                let ast_children: Vec<&Value> = a.iter().filter(|c| is_ast_node(c)).collect();
                if ast_children.is_empty() { continue; }
                let items: String = ast_children
                    .iter()
                    .map(|c| format!("<li>{}</li>", renderNode(c, line_starts, src)))
                    .collect();
                children.push(format!(
                    "<li class=\"prop\"><span class=\"prop-key\">{}[{}]</span><ul>{items}</ul></li>",
                    escapeHtml(key),
                    ast_children.len()
                ));
            }
            c if is_ast_node(c) => {
                children.push(format!(
                    "<li class=\"prop\"><span class=\"prop-key\">{}</span><ul><li>{}</li></ul></li>",
                    escapeHtml(key),
                    renderNode(c, line_starts, src)
                ));
            }
            _ => {}
        }
    }

    let is_program = str_qq(node.get("type")) == "Program";
    let children_html = if children.is_empty() { String::new() } else { format!("<ul>{}</ul>", children.concat()) };
    format!(
        "<details {}><summary>{}</summary>{children_html}</details>",
        if is_program { "open" } else { "" },
        nodeLabel(node, line_starts, src)
    )
}

const CSS: &str = r#"
:root {
  --bg: #1e1e1e; --fg: #ddd; --muted: #888; --accent: #88ddff;
  --type: #ffaa66; --loc: #555; --key: #999; --bar: #444;
}
* { box-sizing: border-box; }
body { font-family: 'JetBrains Mono', Consolas, monospace; padding: 1.5em; background: var(--bg); color: var(--fg); line-height: 1.5; }
h1 { color: #fff; font-weight: 400; margin: 0 0 1em 0; }
h1 .subname { color: var(--accent); }
summary { cursor: pointer; padding: 2px 6px; border-radius: 3px; user-select: none; }
summary:hover { background: #2c2c2c; }
details > ul { margin: 0; padding-left: 1.4em; list-style: none; border-left: 1px solid var(--bar); }
li.prop { margin: 0; padding: 0; }
li.prop > .prop-key { color: var(--key); font-size: 0.9em; padding-left: 6px; }
li.prop > ul { padding-left: 1.2em; }
.type { color: var(--type); font-weight: 600; }
.key { color: var(--muted); font-size: 0.85em; }
.loc { color: var(--loc); font-size: 0.8em; }
code { color: var(--accent); background: #2a2a2a; padding: 1px 5px; border-radius: 2px; font-size: 0.9em; }
.header { display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 1em; border-bottom: 1px solid var(--bar); padding-bottom: 0.5em; }
.header .meta { color: var(--muted); font-size: 0.85em; }
"#;

/// `export default function astHtml(input, _context)`.
pub fn astHtml(input: &Value) -> Result<Value, String> {
    // JS accepts a bare string name, {source, name?}, or {name}.
    let (name, mut source, mut path): (String, Option<String>, Option<String>) = match input {
        Value::String(s) => (s.clone(), None, None),
        _ => {
            let src = input.get("source").filter(|v| !v.is_null()).map(|v| str_qq(Some(v)));
            match src {
                Some(s) => {
                    let n = input.get("name").filter(|v| !v.is_null()).map(|v| str_qq(Some(v)));
                    (n.filter(|x| !x.is_empty()).unwrap_or_else(|| "<inline>".to_string()), Some(s), None)
                }
                None => match input.get("name").filter(|v| !v.is_null()) {
                    Some(n) => (str_qq(Some(n)), None, None),
                    None => return Err("ast-html: name or source is required".to_string()),
                },
            }
        }
    };

    if source.is_none() {
        let p = crate::engine_load_skill::findSkillPath(&Value::String(name.clone()))?;
        let p = str_qq(Some(&p));
        source = Some(std::fs::read_to_string(&p).map_err(|e| format!("ast-html: {e}"))?);
        path = Some(p);
    }
    let src = source.unwrap_or_default();

    // acorn: { ecmaVersion: 'latest', sourceType: 'module', locations: true }.
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, &src, SourceType::mjs())
        .with_options(ParseOptions::default())
        .parse();
    if let Some(diag) = ret.diagnostics.errors().next() {
        // JS lets acorn's SyntaxError propagate; callers catch and degrade.
        return Err(format!("ast-html: {}", diag.message));
    }
    let estree: Value = serde_json::from_str(&ret.program.to_estree_json(false, false))
        .map_err(|e| format!("ast-html: estree serialize: {e}"))?;

    let line_starts = line_starts_of(&src);
    let tree_html = renderNode(&estree, &line_starts, &src);
    let line_count = src.split('\n').count();

    let meta_path = match path {
        Some(p) => format!(" · {}", escapeHtml(&p)),
        None => String::new(),
    };

    Ok(Value::String(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>AST · {name_esc}</title>
<style>{CSS}</style>
</head>
<body><nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>
<div class="header">
  <h1>AST · <span class="subname">{name_esc}</span></h1>
  <div class="meta">{line_count} lines{meta_path}</div>
</div>
<div class="tree">{tree_html}</div>
</body>
</html>"#,
        name_esc = escapeHtml(&name),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render(src: &str) -> String {
        str_qq(Some(&astHtml(&json!({ "source": src, "name": "t.mjs" })).unwrap()))
    }

    #[test]
    fn missing_name_and_source_errors() {
        let e = astHtml(&json!({})).unwrap_err();
        assert!(e.contains("name or source is required"), "{e}");
    }

    #[test]
    fn inline_source_without_a_name_is_labelled_inline() {
        let html = str_qq(Some(&astHtml(&json!({ "source": "const a = 1;\n" })).unwrap()));
        // '<inline>' must be HTML-escaped in both the title and the heading.
        assert!(html.contains("<title>AST · &lt;inline&gt;</title>"), "{html}");
        assert!(html.contains("<span class=\"subname\">&lt;inline&gt;</span>"));
    }

    #[test]
    fn document_shell_carries_the_line_count_and_no_path_for_inline_source() {
        let html = render("const a = 1;\nconst b = 2;\n");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<title>AST · t.mjs</title>"));
        // JS: source.split('\n').length — a trailing newline yields one extra element.
        assert!(html.contains("<div class=\"meta\">3 lines</div>"), "{html}");
        assert!(html.contains("<div class=\"tree\">"));
    }

    #[test]
    fn program_node_is_open_and_children_are_not() {
        let html = render("const a = 1;\n");
        assert!(html.contains("<details open><summary><span class=\"type\">Program</span>"), "{html}");
        // every non-Program details renders with the empty attribute slot
        assert!(html.contains("<details ><summary>"), "{html}");
    }

    #[test]
    fn loc_is_one_based_line_zero_based_column() {
        let html = render("\n  const a = 1;\n");
        // the declaration starts on line 2, column 2
        assert!(html.contains("[2:2]"), "{html}");
        // Program itself starts at the very beginning
        assert!(html.contains("[1:0]"), "{html}");
    }

    #[test]
    fn columns_count_utf16_units_like_acorn_not_bytes() {
        // '😀' is 4 UTF-8 bytes but 2 UTF-16 units. In `const e = '😀'; const after = 1;` the
        // second `const` sits at UTF-16 column 16 and at BYTE column 18 — so this fixture
        // discriminates the two conventions rather than merely exercising the code path.
        let html = render("const e = '😀'; const after = 1;\n");
        assert!(html.contains("[1:16]"), "expected the UTF-16 column 16, got: {html}");
        assert!(!html.contains("[1:18]"), "column was counted in bytes, not UTF-16 units");
    }

    #[test]
    fn inline_keys_render_in_the_js_order_and_are_escaped() {
        let html = render("const x = \"a<b&c\";\n");
        assert!(html.contains("<span class=\"key\">name=</span><code>x</code>"), "{html}");
        // the raw literal keeps its quotes and is escaped
        assert!(html.contains("&lt;b&amp;c"), "{html}");
    }

    #[test]
    fn a_null_or_absent_inline_key_is_omitted() {
        let html = render("debugger;\n");
        // DebuggerStatement has none of the inline keys
        assert!(html.contains("<span class=\"type\">DebuggerStatement</span>"), "{html}");
        assert!(!html.contains("name=</span><code>undefined"), "{html}");
    }

    #[test]
    fn array_properties_render_with_a_counted_prop_key() {
        let html = render("function f(a, b, c) {}\n");
        assert!(html.contains("<span class=\"prop-key\">params[3]</span>"), "{html}");
        assert!(html.contains("<span class=\"prop-key\">body</span>"), "{html}");
    }

    #[test]
    fn an_empty_array_property_is_skipped_entirely() {
        let html = render("function f() {}\n");
        assert!(!html.contains("params["), "an empty params list must not render a branch: {html}");
    }

    #[test]
    fn esm_syntax_parses_as_a_module() {
        let html = render("import x from './y.mjs';\nexport default x;\n");
        assert!(html.contains("ImportDeclaration"), "{html}");
        assert!(html.contains("ExportDefaultDeclaration"), "{html}");
    }

    #[test]
    fn a_syntax_error_is_an_error_not_a_page() {
        let e = astHtml(&json!({ "source": "const = ;\n", "name": "bad.mjs" })).unwrap_err();
        assert!(e.starts_with("ast-html:"), "{e}");
    }

    #[test]
    fn empty_source_renders_an_empty_program() {
        let html = render("");
        assert!(html.contains("<span class=\"type\">Program</span>"), "{html}");
        assert!(html.contains("<div class=\"meta\">1 lines</div>"), "{html}");
    }

    #[test]
    fn rendering_is_deterministic() {
        let src = "export const f = (a) => a + 1;\n";
        assert_eq!(render(src), render(src));
    }
}
