//! jsx-to-html.rs — twin of skills/codebase/jsx-to-html.mjs. Walks the tree-sitter CST of one
//! file (via `crate::codebase_parse_treesitter::parseToTree`, tsx grammar) to find the named
//! component (a Capitalized function/const => with JSX) and emit STATIC HTML: lowercase tags →
//! real tags (class/style/simple attrs kept, handlers/dynamic dropped), text → text, `{expr}` →
//! a placeholder, nested Capitalized components → a labeled stub box.
//!
//! NAMES ARE CONTRACT: `convertJsx`, `jsxToHtml` — kept exactly as the JS source names them.
//!
//! `openTag`'s JS quirk (deliberate, not a bug): a nameless element (a `<>` fragment that slips
//! through as a bare `jsx_opening_element`) computes its tag as the literal STRING `"null"` (via
//! `String(null).toLowerCase()`), never a falsy value — so the element/self-closing renderers
//! always treat it as present and emit `<null>…</null>`, which `convertJsx` then strips via the
//! regex `/<\/?(null|undefined)>/g`. This twin's `open_tag` returns that same `"null"` string in
//! the equivalent case (never an empty tag) to match byte-for-byte, and `strip_null_tags` mirrors
//! the same regex at the end of `convert_jsx`.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;
use tree_sitter::Node;

fn is_cap(s: &str) -> bool {
    s.chars().next().map_or(false, |c| c.is_ascii_uppercase())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn text(n: Node, src: &[u8]) -> String {
    n.utf8_text(src).unwrap_or("").to_string()
}

fn named_children<'tree>(n: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = n.walk();
    n.named_children(&mut cursor).collect()
}

// field(n, f, i) = n.childForFieldName(f) ?? n.namedChildren[i] ?? null
fn field<'tree>(n: Node<'tree>, f: &str, i: usize) -> Option<Node<'tree>> {
    n.child_by_field_name(f).or_else(|| named_children(n).get(i).copied())
}

const JSX_ROOT: [&str; 3] = ["jsx_element", "jsx_self_closing_element", "jsx_fragment"];

fn ws_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\s+").unwrap())
}
fn allowed_attr_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^(class|id|src|alt|type|placeholder|href|title|role|width|height|name|value|aria-\w+|data-\w+)$").unwrap()
    })
}
fn px_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(width|height|top|left|right|bottom|padding|margin|gap|font-size|border-radius)").unwrap())
}
fn strip_null_tags_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"</?(null|undefined)>").unwrap())
}

fn is_event_handler(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with("on") && b.len() > 2 && (b[2] as char).is_ascii_uppercase()
}

fn camel_to_kebab(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

// the opening element -> (tag, attrs(html string), isComp). See module doc for the "null" quirk.
fn open_tag(el: Node, src: &[u8]) -> (String, String, bool) {
    let name_node = named_children(el)
        .into_iter()
        .find(|c| matches!(c.kind(), "identifier" | "member_expression" | "nested_identifier"));
    let raw_tag: Option<String> = name_node.map(|n| text(n, src));
    let is_comp = raw_tag.as_deref().map_or(false, |rt| {
        let last = rt.rsplit('.').next().unwrap_or(rt);
        is_cap(last)
    });

    let mut attrs = String::new();
    for a in named_children(el) {
        if a.kind() != "jsx_attribute" {
            continue;
        }
        let ch = named_children(a);
        let an = ch.get(0).map(|n| text(*n, src));
        let av = ch.get(1).copied();
        let an = match an {
            Some(ref s) if !s.is_empty() => s.clone(),
            _ => continue,
        };
        if is_event_handler(&an) {
            continue;
        }
        let name = if an == "className" { "class".to_string() } else { an };
        let Some(av) = av else {
            attrs.push_str(&format!(" {name}"));
            continue;
        };
        if av.kind() == "string" {
            let frag = named_children(av)
                .into_iter()
                .find(|c| c.kind() == "string_fragment")
                .map(|c| text(c, src))
                .unwrap_or_default();
            if allowed_attr_re().is_match(&name) {
                attrs.push_str(&format!(" {name}=\"{}\"", esc(&frag)));
            }
        } else if av.kind() == "jsx_expression" && name == "style" {
            let css = style_obj_to_css(av, src);
            if !css.is_empty() {
                attrs.push_str(&format!(" style=\"{}\"", esc(&css)));
            }
        }
    }

    let tag = match raw_tag {
        None => "null".to_string(),
        Some(rt) if is_comp => rt,
        Some(rt) => rt.to_lowercase(),
    };
    (tag, attrs, is_comp)
}

// style={{ color: 'red', padding: 13 }} -> "color:red;padding:13px" (literals only)
fn style_obj_to_css(expr_node: Node, src: &[u8]) -> String {
    let Some(obj) = named_children(expr_node).into_iter().find(|c| c.kind() == "object") else {
        return String::new();
    };
    let mut decls: Vec<String> = Vec::new();
    for pair in named_children(obj) {
        if pair.kind() != "pair" {
            continue;
        }
        let (Some(k), Some(v)) = (field(pair, "key", 0), field(pair, "value", 1)) else {
            continue;
        };
        let prop = camel_to_kebab(&text(k, src));
        if v.kind() == "string" {
            let f = named_children(v)
                .into_iter()
                .find(|c| c.kind() == "string_fragment")
                .map(|c| text(c, src))
                .unwrap_or_default();
            if !f.is_empty() {
                decls.push(format!("{prop}:{f}"));
            }
        } else if v.kind() == "number" {
            let vtext = text(v, src);
            let px = if px_re().is_match(&prop) { "px" } else { "" };
            decls.push(format!("{prop}:{vtext}{px}"));
        }
    }
    decls.join(";")
}

fn jsx_to_html_node(node: Node, src: &[u8], depth: i32) -> String {
    if depth > 40 {
        return String::new();
    }
    match node.kind() {
        "jsx_text" => {
            let collapsed = ws_re().replace_all(&text(node, src), " ").to_string();
            if collapsed.trim().is_empty() { String::new() } else { esc(&collapsed) }
        }
        "jsx_expression" => {
            let str_node = named_children(node).into_iter().find(|c| c.kind() == "string");
            if let Some(sn) = str_node {
                let frag = named_children(sn)
                    .into_iter()
                    .find(|c| c.kind() == "string_fragment")
                    .map(|c| text(c, src))
                    .unwrap_or_default();
                esc(&frag)
            } else {
                let compact = ws_re().replace_all(&text(node, src), "").to_string();
                if compact.chars().count() > 2 {
                    "<span class=\"ds-dyn\">…</span>".to_string()
                } else {
                    String::new()
                }
            }
        }
        "jsx_self_closing_element" => {
            let (tag, attrs, is_comp) = open_tag(node, src);
            if is_comp {
                format!("<div class=\"ds-stub\" data-c=\"{0}\">⟨{0}⟩</div>", esc(&tag))
            } else {
                format!("<{tag}{attrs}></{tag}>")
            }
        }
        "jsx_element" | "jsx_fragment" => {
            let kids: Vec<Node> = named_children(node)
                .into_iter()
                .filter(|c| c.kind() != "jsx_opening_element" && c.kind() != "jsx_closing_element")
                .collect();
            let inner: String = kids.iter().map(|c| jsx_to_html_node(*c, src, depth + 1)).collect();
            if node.kind() == "jsx_fragment" {
                return inner;
            }
            let open = named_children(node).into_iter().find(|c| c.kind() == "jsx_opening_element");
            let Some(open) = open else {
                return inner;
            };
            let (tag, attrs, is_comp) = open_tag(open, src);
            if is_comp {
                let esc_tag = esc(&tag);
                return if inner.is_empty() {
                    format!("<div class=\"ds-stub\" data-c=\"{esc_tag}\">⟨{esc_tag}⟩</div>")
                } else {
                    format!("<div class=\"ds-stub\" data-c=\"{esc_tag}\">{inner}</div>")
                };
            }
            format!("<{tag}{attrs}>{inner}</{tag}>")
        }
        _ => String::new(),
    }
}

fn find_jsx_root<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    if JSX_ROOT.contains(&node.kind()) {
        return Some(node);
    }
    for c in named_children(node) {
        if let Some(r) = find_jsx_root(c) {
            return Some(r);
        }
    }
    None
}

struct FoundComponent<'tree> {
    jsx: Node<'tree>,
    name: String,
}

// preorder DFS: the first node whose {nm, isCap(nm), body, JSX-in-body} all hold wins (matches
// JS's walk() which visits every node but no-ops once `found` is set).
fn find_component_jsx<'tree>(node: Node<'tree>, name: Option<&str>, src: &[u8]) -> Option<FoundComponent<'tree>> {
    let mut nm: Option<String> = None;
    let mut body: Option<Node<'tree>> = None;
    if node.kind() == "function_declaration" {
        nm = field(node, "name", 0).map(|n| text(n, src));
        body = Some(node);
    } else if node.kind() == "variable_declarator" {
        if let Some(val) = field(node, "value", 1) {
            if matches!(val.kind(), "arrow_function" | "function" | "function_expression") {
                nm = field(node, "name", 0).map(|n| text(n, src));
                body = Some(val);
            }
        }
    }
    if let (Some(nm_s), Some(body_n)) = (nm, body) {
        if is_cap(&nm_s) && name.map_or(true, |n| n == nm_s) {
            if let Some(jsx) = find_jsx_root(body_n) {
                return Some(FoundComponent { jsx, name: nm_s });
            }
        }
    }
    for c in named_children(node) {
        if let Some(r) = find_component_jsx(c, name, src) {
            return Some(r);
        }
    }
    None
}

/// convertJsx(source, name) -> { ok, name, html }. No-db helper, 2 positional args (name may be
/// absent/null -> None, matching JS `name ?? null`).
#[allow(non_snake_case)]
pub fn convertJsx(source: &Value, name: &Value) -> Value {
    let source_s = source.as_str().unwrap_or("").to_string();
    let name_s: Option<String> = name.as_str().map(|s| s.to_string());
    let name_out: Value = match &name_s {
        Some(s) => json!(s),
        None => Value::Null,
    };

    let Some(tree) = crate::codebase_parse_treesitter::parseToTree(&source_s, "tsx") else {
        return json!({ "ok": false, "name": name_out, "html": "" });
    };
    let root = tree.root_node();
    let src = source_s.as_bytes();

    let Some(found) = find_component_jsx(root, name_s.as_deref(), src) else {
        return json!({ "ok": false, "name": name_out, "html": "" });
    };

    let html_raw = jsx_to_html_node(found.jsx, src, 0);
    let html = strip_null_tags_re().replace_all(&html_raw, "").to_string();
    json!({ "ok": true, "name": found.name, "html": html })
}

/// jsxToHtml(input, context) — default export. `source` required (`input?.source == null` throws).
#[allow(non_snake_case)]
pub fn jsxToHtml(input: &Value, _db: &Connection) -> Result<Value, String> {
    let source = input.get("source").filter(|v| !v.is_null());
    let Some(source) = source else {
        return Err("jsx-to-html: source is required".to_string());
    };
    let name = input.get("name").cloned().unwrap_or(Value::Null);
    Ok(convertJsx(source, &name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_literal_styles_dynamic_and_nested_component_stub() {
        let src = r#"function Card({ title }) {
    return <div className="card" style={{ padding: 12, color: 'red' }}>
      <h2 className="title">{title}</h2>
      <Icon name="x" />
      <span>hello</span>
    </div>;
  }"#;
        let result = convertJsx(&json!(src), &json!("Card"));
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["name"], json!("Card"));
        let html = result["html"].as_str().unwrap();
        assert!(html.contains(r#"<div class="card" style="padding:12px;color:red">"#));
        assert!(html.contains(r#"<h2 class="title"><span class="ds-dyn">…</span></h2>"#));
        assert!(html.contains(r#"<div class="ds-stub" data-c="Icon">"#));
        assert!(html.contains("<span>hello</span>"));
    }

    #[test]
    fn no_component_found_returns_ok_false_with_requested_name() {
        let result = convertJsx(&json!("const x = 1;"), &json!("Missing"));
        assert_eq!(result, json!({ "ok": false, "name": "Missing", "html": "" }));
    }

    #[test]
    fn jsx_to_html_requires_source() {
        let conn = Connection::open_in_memory().unwrap();
        let err = jsxToHtml(&json!({}), &conn).unwrap_err();
        assert!(err.contains("source"));
    }

    #[test]
    fn jsx_to_html_delegates_to_convert_jsx() {
        let conn = Connection::open_in_memory().unwrap();
        let src = "function Foo() { return <span>hi</span>; }";
        let result = jsxToHtml(&json!({ "source": src }), &conn).unwrap();
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["name"], json!("Foo"));
        assert_eq!(result["html"], json!("<span>hi</span>"));
    }
}
