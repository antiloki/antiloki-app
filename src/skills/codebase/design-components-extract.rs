//! design-components-extract.rs — twin of skills/codebase/design-components-extract.mjs. NAMES
//! ARE CONTRACT: `extractComponents`, `writeComponents`, `designComponentsExtract`. A faithful
//! tree-sitter CST walk (via the already-converted `codebase_parse_treesitter::parseToTree`
//! twin, tsx/typescript grammars only — the same scope that twin ports) that finds a Capitalized
//! `function Foo(…)` / `const Foo = (…) =>` whose body contains JSX, and records its destructured
//! props, the child components it renders, and the `C.*`-shaped design tokens its `style={{...}}`
//! object literals consume.
//!
//! `extractComponents(root, path)` is TRANSITIVE like `parseToTree`: its first arg is a
//! tree-sitter `Node<'tree>`, not a `serde_json::Value` — no JSON fixture can construct it, same
//! reasoning as `parseToTree`'s own transitive classification in parse-treesitter.rs. Unlike the
//! JS source (whose bindings embed `.text` on every node), native tree-sitter requires the source
//! bytes explicitly — this twin's signature adds a trailing `src: &[u8]` for that reason alone
//! (documented Rust-only difference, same category as `parseToTree` returning `Tree` not `Node`).
//!
//! `designComponentsExtract`'s `{all:true}` sweep mode reads the file list from `code_nodes` (db)
//! then the REAL file content of each via the already-converted `codebase_codebase_file_read`
//! twin — deterministic given file bytes, fixture-controlled the same way stack-facts.rs is: seed
//! an active `workspaces` row whose `root_path` points at a real on-disk testdata dir committed
//! alongside this fixture (`antiloki-crossing/fixtures/testdata/design-components-extract-basic/`).
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::OnceLock;
use tree_sitter::Node;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

fn ext_of(p: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"\.[^./\\]+$").unwrap());
    re.find(p).map(|m| m.as_str().to_lowercase()).unwrap_or_default()
}
fn grammar_for(p: &str) -> Option<&'static str> {
    match ext_of(p).as_str() {
        ".tsx" | ".jsx" => Some("tsx"),
        ".ts" | ".mts" | ".cts" => Some("typescript"),
        _ => None,
    }
}
fn is_cap(s: &str) -> bool { s.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) }

fn jsx_set() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| ["jsx_element", "jsx_self_closing_element", "jsx_opening_element", "jsx_fragment"].into_iter().collect())
}

fn text(n: Node, src: &[u8]) -> String { n.utf8_text(src).unwrap_or("").to_string() }

fn named_children<'tree>(n: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = n.walk();
    n.named_children(&mut cursor).collect()
}

fn walk_cst<'a>(node: Node<'a>, visit: &mut impl FnMut(Node<'a>)) {
    visit(node);
    for c in named_children(node) { walk_cst(c, visit); }
}

fn field<'a>(n: Node<'a>, name: &str, idx: u32) -> Option<Node<'a>> {
    n.child_by_field_name(name).or_else(|| n.named_child(idx))
}

fn subtree_has_jsx(node: Node) -> bool {
    let mut found = false;
    walk_cst(node, &mut |n| { if jsx_set().contains(n.kind()) { found = true; } });
    found
}

fn collect_props(params_node: Option<Node>, src: &[u8]) -> Vec<String> {
    let mut props: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let push = |props: &mut Vec<String>, seen: &mut HashSet<String>, t: String| { if seen.insert(t.clone()) { props.push(t); } };
    if let Some(pn) = params_node {
        walk_cst(pn, &mut |n| match n.kind() {
            "shorthand_property_identifier_pattern" => push(&mut props, &mut seen, text(n, src)),
            "pair_pattern" => { if let Some(k) = field(n, "key", 0) { push(&mut props, &mut seen, text(k, src)); } }
            "rest_pattern" => { if let Some(id) = named_children(n).into_iter().next() { push(&mut props, &mut seen, format!("…{}", text(id, src))); } }
            _ => {}
        });
    }
    props
}

fn collect_renders_and_tokens(body: Node, src: &[u8]) -> (Vec<String>, Vec<String>) {
    let mut renders: Vec<String> = Vec::new();
    let mut renders_seen: HashSet<String> = HashSet::new();
    let mut tokens: Vec<String> = Vec::new();
    let mut tokens_seen: HashSet<String> = HashSet::new();
    walk_cst(body, &mut |n| {
        if n.kind() == "jsx_opening_element" || n.kind() == "jsx_self_closing_element" {
            let tag = named_children(n).into_iter().find(|c| matches!(c.kind(), "identifier" | "member_expression" | "nested_identifier"));
            if let Some(tag) = tag {
                let tag_text = text(tag, src);
                let last_seg = tag_text.rsplit('.').next().unwrap_or(&tag_text).to_string();
                if is_cap(&last_seg) && renders_seen.insert(tag_text.clone()) { renders.push(tag_text); }
            }
        }
        if n.kind() == "jsx_attribute" {
            let nc = named_children(n);
            if nc.first().map(|c| text(*c, src)).as_deref() == Some("style") {
                if let Some(val_node) = nc.get(1) {
                    if let Some(obj) = named_children(*val_node).into_iter().find(|c| c.kind() == "object") {
                        for pair in named_children(obj) {
                            if pair.kind() != "pair" { continue; }
                            if let Some(val) = field(pair, "value", 1) {
                                if val.kind() == "member_expression" {
                                    let vt = text(val, src);
                                    if tokens_seen.insert(vt.clone()) { tokens.push(vt); }
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    (renders, tokens)
}

/// Pure: given a parsed CST root, emit the component inventory for one file.
pub fn extractComponents(root: Option<Node>, path: &str, src: &[u8]) -> Vec<Value> {
    let Some(root) = root else { return Vec::new() };
    let mut comps: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    walk_cst(root, &mut |node| {
        let mut name: Option<String> = None;
        let mut params_node: Option<Node> = None;
        let mut body_node: Option<Node> = None;
        if node.kind() == "function_declaration" {
            name = field(node, "name", 0).map(|n| text(n, src));
            params_node = field(node, "parameters", 1);
            body_node = Some(node);
        } else if node.kind() == "variable_declarator" {
            if let Some(val) = field(node, "value", 1) {
                if matches!(val.kind(), "arrow_function" | "function" | "function_expression") {
                    name = field(node, "name", 0).map(|n| text(n, src));
                    params_node = named_children(val).into_iter().find(|c| c.kind() == "formal_parameters");
                    body_node = Some(val);
                }
            }
        }
        let name = match name.filter(|n| !n.is_empty()) { Some(n) if is_cap(&n) => n, _ => return };
        let Some(body) = body_node else { return };
        if !subtree_has_jsx(body) { return; }
        let line = node.start_position().row as i64 + 1;
        let key = format!("{name}@{line}");
        if !seen.insert(key) { return; }
        let (renders, tokens) = collect_renders_and_tokens(body, src);
        comps.push(json!({
            "name": name, "path": path, "line": line,
            "props": collect_props(params_node, src),
            "renders": renders, "tokens": tokens,
        }));
    });
    comps
}

/// TWO-LANE rebuild, finalize: persist the pooled per-family component inventory (wipe+repopulate).
pub fn writeComponents(db: &Connection, origin: &Value, comps: &Value) -> Value {
    let origin_s = str_qq(Some(origin));
    let comps_arr: Vec<Value> = comps.as_array().cloned().unwrap_or_default();
    let mut persisted = false;
    if crate::db_json::run_write(db, "DELETE FROM code_design_components WHERE origin = ?", &[json!(origin_s)]).is_ok() {
        persisted = true;
    }
    if persisted {
        for c in &comps_arr {
            let _ = crate::db_json::run_write(
                db,
                "INSERT INTO code_design_components (origin, name, path, line, props_json, renders_json, tokens_json) VALUES (?,?,?,?,?,?,?)",
                &[
                    json!(origin_s),
                    c.get("name").cloned().unwrap_or(Value::Null),
                    c.get("path").cloned().unwrap_or(Value::Null),
                    c.get("line").cloned().unwrap_or(Value::Null),
                    json!(serde_json::to_string(c.get("props").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into())),
                    json!(serde_json::to_string(c.get("renders").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into())),
                    json!(serde_json::to_string(c.get("tokens").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into())),
                ],
            );
        }
    }
    json!({ "ok": true, "origin": origin_s, "components": comps_arr.len() as i64, "persisted": persisted })
}

pub fn designComponentsExtract(input: &Value, db: &Connection) -> Result<Value, String> {
    let path_val = input.get("path").and_then(|v| v.as_str());

    if input.get("source").filter(|v| !v.is_null()).is_some() {
        let src_str = str_qq(input.get("source"));
        let path_for_extract = path_val.map(|s| s.to_string()).unwrap_or_else(|| "(source)".to_string());
        let path_for_grammar = path_val.unwrap_or(".tsx");
        let grammar = grammar_for(path_for_grammar).unwrap_or("tsx");
        let tree = crate::codebase_parse_treesitter::parseToTree(&src_str, grammar);
        let root = tree.as_ref().map(|t| t.root_node());
        let comps = extractComponents(root, &path_for_extract, src_str.as_bytes());
        return Ok(json!({ "ok": true, "components": comps }));
    }

    if truthy(input.get("all")) {
        let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
        let rows = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin = ? AND type = 'file' ORDER BY path", &[json!(origin)])?;
        let files: Vec<String> = rows.as_array().cloned().unwrap_or_default().into_iter()
            .filter_map(|r| r.get("path").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .filter(|p| grammar_for(p).is_some())
            .collect();

        let mut all: Vec<Value> = Vec::new();
        let mut parsed: i64 = 0;
        for f in &files {
            let content = match crate::codebase_codebase_file_read::codebaseFileRead(&json!({ "path": f }), db) {
                Ok(v) => str_qq(v.get("content")),
                Err(_) => continue,
            };
            if content.is_empty() { continue; }
            let Some(grammar) = grammar_for(f) else { continue };
            let Some(tree) = crate::codebase_parse_treesitter::parseToTree(&content, grammar) else { continue };
            parsed += 1;
            let comps = extractComponents(Some(tree.root_node()), f, content.as_bytes());
            all.extend(comps);
        }

        let w = writeComponents(db, &json!(origin), &json!(all));
        let mut obj = w.as_object().cloned().unwrap_or_default();
        obj.insert("files".into(), json!(parsed));
        return Ok(Value::Object(obj));
    }

    Err("design-components-extract: source|all is required".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "function Foo({ a, b }) { return <div><Bar x={1} /></div>; }\nconst Baz = ({ x }: Props) => <Foo style={{ color: C.brand, padding: 13 }}><Qux/></Foo>;\nconst notComp = 5;\nfunction helper() { return <div/>; }";

    #[test]
    fn extracts_components_capitalized_only_with_jsx() {
        let tree = crate::codebase_parse_treesitter::parseToTree(SRC, "tsx").unwrap();
        let comps = extractComponents(Some(tree.root_node()), "a.tsx", SRC.as_bytes());
        let foo = comps.iter().find(|c| c["name"] == "Foo").unwrap();
        let mut props = foo["props"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect::<Vec<_>>();
        props.sort();
        assert_eq!(props, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(foo["renders"], json!(["Bar"]));

        let baz = comps.iter().find(|c| c["name"] == "Baz").unwrap();
        assert_eq!(baz["props"], json!(["x"]));
        let mut renders = baz["renders"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect::<Vec<_>>();
        renders.sort();
        assert_eq!(renders, vec!["Foo".to_string(), "Qux".to_string()]);
        assert_eq!(baz["tokens"], json!(["C.brand"]));

        assert!(comps.iter().find(|c| c["name"] == "notComp").is_none());
        assert!(comps.iter().find(|c| c["name"] == "helper").is_none());
    }

    #[test]
    fn write_components_wipes_and_repopulates_by_origin() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_design_components (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL, line INTEGER, props_json TEXT NOT NULL DEFAULT '[]', renders_json TEXT NOT NULL DEFAULT '[]', tokens_json TEXT NOT NULL DEFAULT '[]', created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')").unwrap();
        let comps = json!([{ "name": "Foo", "path": "a.tsx", "line": 1, "props": ["x"], "renders": ["Bar"], "tokens": ["C.brand"] }]);
        let w = writeComponents(&c, &json!("atlas"), &comps);
        assert_eq!(w["ok"], json!(true));
        assert_eq!(w["persisted"], json!(true));
        assert_eq!(w["components"], json!(1));
    }

    #[test]
    fn design_components_extract_source_mode() {
        let c = Connection::open_in_memory().unwrap();
        let out = designComponentsExtract(&json!({ "source": SRC, "path": "a.tsx" }), &c).unwrap();
        assert_eq!(out["ok"], json!(true));
        assert!(out["components"].as_array().unwrap().len() >= 2);
    }
}
