//! design-tokens-extract.rs — twin of skills/codebase/design-tokens-extract.mjs. NAMES ARE
//! CONTRACT: `grammarFor`, `classifyToken`, `extractStyleTokens`, `extractCssVars`,
//! `extractCssTokens`, `accumulate`, `extractTokensFromTree`, `finalizeTokens`,
//! `designTokensExtract` (default).
//!
//! Harvests the de-facto DESIGN PALETTE from the real UI: declared theme tokens
//! (`theme.ts: const C = { brand: "oklch(…)" }`), style-literal drift candidates
//! (`style={{ color: "#fff" }}`), and CSS/SCSS declarations (stylesheets, `<style>` blocks,
//! CSS-in-JS, Angular inline styles via the already-converted `codebase_style_locate::locateStyles`
//! twin). BESPOKE-NATIVE for the same reason as `design-components-extract.rs`: `extractStyleTokens`
//! is TRANSITIVE — its root arg is a tree-sitter `Node<'tree>` (via the already-converted
//! `codebase_parse_treesitter::parseToTree` twin), not a `serde_json::Value`, so no JSON dispatch
//! fixture can construct it. The `{all:true}` sweep mode does real work: reads `code_nodes` (db),
//! the real file bytes of each via `codebase_codebase_file_read::codebaseFileRead`, and — in
//! `finalizeTokens` — a REAL recursive filesystem walk (`walkStyleFiles`, budget-capped at 8000
//! dirs/files, same cap as the JS) rooted at `code_onboard_jobs.root`, then persists via
//! `code_design_tokens` DELETE+INSERT (wipe+repopulate per origin), same pattern as
//! `writeComponents` in design-components-extract.rs.
//!
//! `classifyToken(prop, value)` takes `&str` here (not `&Value`) — this fn is pure logic called
//! internally with plain JS strings in the source (`key.text`, never a JSON-shaped value crossing
//! a dispatch boundary), so the Rust-idiomatic signature is a faithful, testable equivalent; same
//! reasoning `design-components-extract.rs` gives for its `Node`/`&[u8]` signatures.
//!
//! Progress yielding (`context?.onProgress?.(...)` + `await new Promise(setImmediate)` every 25
//! files in the JS sweep) has no Rust equivalent — this twin's sweep runs synchronously to
//! completion. Documented Rust-only difference (no `context` object with a progress callback
//! exists on this side); the OBSERVABLE result (`all` findings, persisted rows, `files` count) is
//! unaffected — only the intermediate progress ticks are dropped.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tree_sitter::{Node, Tree};

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

const STYLE_EXT: [&str; 9] = [".css", ".scss", ".sass", ".less", ".styl", ".vue", ".svelte", ".html", ".htm"];
const STYLE_EXT_EXTRA: &str = ".astro";
const SKIP_DIR: [&str; 9] = ["node_modules", ".git", "dist", "build", "coverage", ".next", "out", ".cache", ".antiloki"];

fn ext_of(p: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"\.[^./\\]+$").unwrap());
    re.find(p).map(|m| m.as_str().to_lowercase()).unwrap_or_default()
}

fn is_style_ext(ext: &str) -> bool {
    STYLE_EXT.contains(&ext) || ext == STYLE_EXT_EXTRA
}

fn walk_style_files(root: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![PathBuf::from(root)];
    let mut budget: i64 = 8000;
    while let Some(dir) = stack.pop() {
        if budget <= 0 {
            break;
        }
        budget -= 1;
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if !SKIP_DIR.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push(path);
                }
            } else if is_style_ext(&ext_of(&name)) {
                out.push(path);
            }
        }
    }
    out
}

/// `export const grammarFor`
pub fn grammarFor(p: &str) -> Option<&'static str> {
    match ext_of(p).as_str() {
        ".tsx" | ".jsx" => Some("tsx"),
        ".ts" | ".mts" | ".cts" => Some("typescript"),
        _ => None,
    }
}

fn is_theme_file(p: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"(?i)(?:^|[\\/])(?:theme|tokens|palette|colors?)\.[mc]?[jt]sx?$").unwrap());
    re.is_match(p)
}

struct TokenRes {
    radius: Regex,
    font_size: Regex,
    font_family_style: Regex,
    font_weight: Regex,
    z: Regex,
    opacity: Regex,
    motion: Regex,
    color_prop: Regex,
    color_value: Regex,
    spacing: Regex,
    font_family_value: Regex,
}
fn token_res() -> &'static TokenRes {
    static R: OnceLock<TokenRes> = OnceLock::new();
    R.get_or_init(|| TokenRes {
        radius: Regex::new(r"(?i)radius").unwrap(),
        font_size: Regex::new(r"(?i)fontSize").unwrap(),
        font_family_style: Regex::new(r"(?i)fontFamily|fontStyle").unwrap(),
        font_weight: Regex::new(r"(?i)fontWeight").unwrap(),
        z: Regex::new(r"(?i)zIndex").unwrap(),
        opacity: Regex::new(r"(?i)opacity").unwrap(),
        motion: Regex::new(r"(?i)transition|animation|duration|easing|delay").unwrap(),
        color_prop: Regex::new(r"(?i)color|background|fill|stroke|^border$|borderColor|boxShadow|shadow|outline").unwrap(),
        color_value: Regex::new(r"(?i)^(#|rgb|hsl|oklch|oklab|color-mix|var\()").unwrap(),
        spacing: Regex::new(r"(?i)padding|margin|gap|top|left|right|bottom|inset|width|height|^size$|flexBasis|lineHeight|letterSpacing").unwrap(),
        font_family_value: Regex::new(r"(?i)monospace|sans-serif|\bserif\b|system-ui|-apple-system|ui-monospace").unwrap(),
    })
}

/// `export function classifyToken(prop, value)`
pub fn classifyToken(prop: &str, value: &str) -> &'static str {
    let p = prop.replace('-', "");
    let r = token_res();
    if r.radius.is_match(&p) {
        return "radius";
    }
    if r.font_size.is_match(&p) {
        return "font_size";
    }
    if r.font_family_style.is_match(&p) {
        return "font_family";
    }
    if r.font_weight.is_match(&p) {
        return "font_weight";
    }
    if r.z.is_match(&p) {
        return "z";
    }
    if r.opacity.is_match(&p) {
        return "opacity";
    }
    if r.motion.is_match(&p) {
        return "motion";
    }
    if r.color_prop.is_match(&p) {
        return "color";
    }
    if r.color_value.is_match(value) {
        return "color";
    }
    if r.spacing.is_match(&p) {
        return "spacing";
    }
    if r.font_family_value.is_match(value) {
        return "font_family";
    }
    "other"
}

fn named_children<'tree>(n: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = n.walk();
    n.named_children(&mut cursor).collect()
}
fn text(n: Node, src: &[u8]) -> String {
    n.utf8_text(src).unwrap_or("").to_string()
}
fn field<'a>(n: Node<'a>, name: &str, idx: u32) -> Option<Node<'a>> {
    n.child_by_field_name(name).or_else(|| n.named_child(idx))
}
fn str_text(n: Node, src: &[u8]) -> String {
    if n.kind() == "string" {
        named_children(n).into_iter().find(|c| c.kind() == "string_fragment").map(|c| text(c, src)).unwrap_or_default()
    } else {
        text(n, src)
    }
}
fn lit_value(n: Node, src: &[u8]) -> Option<String> {
    match n.kind() {
        "string" => Some(str_text(n, src)),
        "number" => Some(text(n, src)),
        _ => None,
    }
}

fn object_of(node: Option<Node>) -> Option<Node> {
    let node = node?;
    if node.kind() == "object" {
        return Some(node);
    }
    if matches!(
        node.kind(),
        "as_expression" | "satisfies_expression" | "parenthesized_expression" | "type_assertion" | "non_null_expression"
    ) {
        for c in named_children(node) {
            if let Some(o) = object_of(Some(c)) {
                return Some(o);
            }
        }
    }
    None
}

fn walk_cst<'a>(node: Node<'a>, visit: &mut impl FnMut(Node<'a>)) {
    visit(node);
    for c in named_children(node) {
        walk_cst(c, visit);
    }
}

/// `export function extractStyleTokens(root, path, opts = {})` — `theme_override` mirrors
/// `opts.theme` (`None` ⇒ `isThemeFile(path)`, matching the JS `opts.theme ?? isThemeFile(path)`).
pub fn extractStyleTokens(root: Option<Node>, path: &str, src: &[u8], theme_override: Option<bool>) -> Vec<Value> {
    let Some(root) = root else { return Vec::new() };
    let mut out: Vec<Value> = Vec::new();
    let theme = theme_override.unwrap_or_else(|| is_theme_file(path));

    walk_cst(root, &mut |node| {
        if node.kind() == "jsx_attribute" {
            let nc = named_children(node);
            if nc.first().map(|c| text(*c, src)).as_deref() == Some("style") {
                if let Some(val_node) = nc.get(1) {
                    if let Some(obj) = named_children(*val_node).into_iter().find(|c| c.kind() == "object") {
                        for pair in named_children(obj) {
                            if pair.kind() != "pair" {
                                continue;
                            }
                            let (Some(key), Some(val)) = (field(pair, "key", 0), field(pair, "value", 1)) else {
                                continue;
                            };
                            let prop = text(key, src);
                            let line = pair.start_position().row as i64 + 1;
                            let lit = lit_value(val, src);
                            if let Some(lit) = lit.filter(|l| !l.is_empty()) {
                                let category = classifyToken(&prop, &lit);
                                if category != "other" {
                                    out.push(json!({"token": Value::Null, "value": lit, "category": category, "declared": 0, "usage": 1, "line": line, "path": path}));
                                }
                            } else if matches!(val.kind(), "member_expression" | "identifier") {
                                out.push(json!({"token": text(val, src), "value": Value::Null, "category": classifyToken(&prop, ""), "declared": 0, "usage": 1, "line": line, "path": path, "isUsage": true}));
                            }
                        }
                    }
                }
            }
        }
        if theme && node.kind() == "variable_declarator" {
            let name_node = field(node, "name", 0);
            let val_node = field(node, "value", 1);
            let (Some(name_node), Some(val_node)) = (name_node, val_node) else { return };
            let base = text(name_node, src);
            if base.is_empty() {
                return;
            }
            if let Some(obj) = object_of(Some(val_node)) {
                for pair in named_children(obj) {
                    if pair.kind() != "pair" {
                        continue;
                    }
                    let (Some(key), Some(val)) = (field(pair, "key", 0), field(pair, "value", 1)) else {
                        continue;
                    };
                    if let Some(lit) = lit_value(val, src) {
                        let key_text = text(key, src);
                        let line = pair.start_position().row as i64 + 1;
                        let category = classifyToken(&key_text, &lit);
                        out.push(json!({"token": format!("{base}.{key_text}"), "value": lit, "category": category, "declared": 1, "usage": 0, "line": line, "path": path}));
                    }
                }
            } else if let Some(lit) = lit_value(val_node, src) {
                let line = node.start_position().row as i64 + 1;
                let category = classifyToken(&base, &lit);
                out.push(json!({"token": base, "value": lit, "category": category, "declared": 1, "usage": 0, "line": line, "path": path}));
            }
        }
    });
    out
}

/// `export function extractCssVars(source, path)`
pub fn extractCssVars(source: &str, path: &str) -> Vec<Value> {
    static ROOT_RE: OnceLock<Regex> = OnceLock::new();
    static VAR_RE: OnceLock<Regex> = OnceLock::new();
    let root_re = ROOT_RE.get_or_init(|| Regex::new(r":root\s*\{([\s\S]*?)\}").unwrap());
    let var_re = VAR_RE.get_or_init(|| Regex::new(r"(--[\w-]+)\s*:\s*([^;]+);").unwrap());
    let mut out = Vec::new();
    let Some(m) = root_re.captures(source) else { return out };
    let block = m.get(1).map(|x| x.as_str()).unwrap_or("");
    for c in var_re.captures_iter(block) {
        let token = c[1].trim().to_string();
        let value = c[2].trim().to_string();
        if !value.is_empty() {
            let category = classifyToken(&token, &value);
            out.push(json!({"token": token, "value": value, "category": category, "declared": 1, "usage": 0, "line": 0, "path": path}));
        }
    }
    out
}

fn is_compound(v: &str) -> bool {
    static STRIP_PARENS: OnceLock<Regex> = OnceLock::new();
    static COMMA: OnceLock<Regex> = OnceLock::new();
    static SHORTHAND: OnceLock<Regex> = OnceLock::new();
    let strip = STRIP_PARENS.get_or_init(|| Regex::new(r"\([^)]*\)").unwrap());
    let comma = COMMA.get_or_init(|| Regex::new(r",").unwrap());
    let shorthand = SHORTHAND.get_or_init(|| Regex::new(r"(?i)\b(solid|dashed|dotted|inset|outset)\b").unwrap());
    comma.is_match(&strip.replace_all(v, "")) || shorthand.is_match(v)
}

/// `export function extractCssTokens(source, path)`
pub fn extractCssTokens(source: &str, path: &str) -> Vec<Value> {
    static COMMENT_BLOCK: OnceLock<Regex> = OnceLock::new();
    static COMMENT_LINE: OnceLock<Regex> = OnceLock::new();
    static IMPORTANT: OnceLock<Regex> = OnceLock::new();
    static WS: OnceLock<Regex> = OnceLock::new();
    static SCSS_VAR: OnceLock<Regex> = OnceLock::new();
    static CSS_VAR: OnceLock<Regex> = OnceLock::new();
    static SCSS_DECL: OnceLock<Regex> = OnceLock::new();
    static DECL: OnceLock<Regex> = OnceLock::new();
    static REF: OnceLock<Regex> = OnceLock::new();
    static COLOR_FN: OnceLock<Regex> = OnceLock::new();

    let comment_block = COMMENT_BLOCK.get_or_init(|| Regex::new(r"(?s)/\*.*?\*/").unwrap());
    let comment_line = COMMENT_LINE.get_or_init(|| Regex::new(r"(^|[^:])//[^\n]*").unwrap());
    let important = IMPORTANT.get_or_init(|| Regex::new(r"(?i)!important").unwrap());
    let ws = WS.get_or_init(|| Regex::new(r"\s+").unwrap());
    let scss_var = SCSS_VAR.get_or_init(|| Regex::new(r"\$([\w-]+)\s*:\s*([^;{}]+);").unwrap());
    let css_var = CSS_VAR.get_or_init(|| Regex::new(r"(--[\w-]+)\s*:\s*([^;{}]+);").unwrap());
    let scss_decl = SCSS_DECL.get_or_init(|| Regex::new(r"\$[\w-]+\s*:\s*[^;{}]+;").unwrap());
    let decl = DECL.get_or_init(|| Regex::new(r"([a-zA-Z][a-zA-Z-]*)\s*:\s*([^;{}@]+);").unwrap());
    let ref_re = REF.get_or_init(|| Regex::new(r"[$]|#\{").unwrap());
    let color_fn = COLOR_FN.get_or_init(|| Regex::new(r"(?i)^(rgb|hsl|oklch|oklab|color-mix)").unwrap());

    let mut out = Vec::new();
    let no_block = comment_block.replace_all(source, "");
    let text = comment_line.replace_all(&no_block, "$1").into_owned();
    let clean = |v: &str| -> String {
        let v = important.replace_all(v, "");
        ws.replace_all(v.trim(), " ").trim().to_string()
    };
    let line_of = |idx: usize| -> i64 { text[..idx].matches('\n').count() as i64 + 1 };

    // 1. SCSS variables → declared tokens
    for c in scss_var.captures_iter(&text) {
        let m = c.get(0).unwrap();
        let v = clean(&c[2]);
        if !v.is_empty() && !ref_re.is_match(&v) && !is_compound(&v) {
            let token = format!("${}", &c[1]);
            let category = classifyToken(&c[1], &v);
            out.push(json!({"token": token, "value": v, "category": category, "declared": 1, "usage": 0, "line": line_of(m.start()), "path": path}));
        }
    }
    // 2. CSS custom properties → declared tokens
    for c in css_var.captures_iter(&text) {
        let m = c.get(0).unwrap();
        let v = clean(&c[2]);
        if !v.is_empty() && !v.starts_with("var(") {
            let token = c[1].to_string();
            let category = classifyToken(&token, &v);
            out.push(json!({"token": token, "value": v, "category": category, "declared": 1, "usage": 0, "line": line_of(m.start()), "path": path}));
        }
    }
    // 3. plain declarations → literals (remove $var declarations first)
    let decl_text = scss_decl.replace_all(&text, "");
    for c in decl.captures_iter(&decl_text) {
        let m = c.get(0).unwrap();
        let prop = c[1].to_lowercase();
        let v = clean(&c[2]);
        if v.is_empty() || ref_re.is_match(&v) || v.contains("var(") || prop.starts_with("--") || is_compound(&v) {
            continue;
        }
        let category = classifyToken(&prop, &v);
        if category == "other" {
            continue;
        }
        if category == "color" && v.contains(' ') && !color_fn.is_match(&v) {
            continue;
        }
        out.push(json!({"token": Value::Null, "value": v, "category": category, "declared": 0, "usage": 1, "line": line_of(m.start()), "path": path}));
    }
    out
}

struct Row {
    token: Value,
    value: Value,
    category: String,
    declared: i64,
    usage_count: i64,
    sources: Vec<Value>,
}

/// `export function accumulate(findings)`
pub fn accumulate(findings: &[Value]) -> Vec<Value> {
    let mut rows: HashMap<String, Row> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut usages: HashMap<String, i64> = HashMap::new();

    for f in findings {
        let is_usage = truthy(f.get("isUsage"));
        if is_usage {
            let token = str_qq(f.get("token"));
            *usages.entry(token).or_insert(0) += 1;
            continue;
        }
        let token_val = f.get("token").cloned().unwrap_or(Value::Null);
        let token_key = str_qq(Some(&token_val));
        let category = str_qq(f.get("category"));
        let value = f.get("value").cloned().unwrap_or(Value::Null);
        let value_key = str_qq(Some(&value));
        let declared = f.get("declared").and_then(Value::as_i64).unwrap_or(0);
        let key = format!("{declared}|{token_key}|{category}|{value_key}");
        let row = rows.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            Row { token: token_val.clone(), value: value.clone(), category: category.clone(), declared, usage_count: 0, sources: Vec::new() }
        });
        row.usage_count += f.get("usage").and_then(Value::as_i64).unwrap_or(1);
        let path = f.get("path").cloned();
        if row.sources.len() < 20 {
            if let Some(path) = path.filter(|p| !p.is_null()) {
                row.sources.push(json!({"path": path, "line": f.get("line").cloned().unwrap_or(Value::Null)}));
            }
        }
    }
    for key in &order {
        let row = rows.get_mut(key).unwrap();
        if row.declared != 0 {
            if let Some(tok) = row.token.as_str() {
                if let Some(u) = usages.get(tok) {
                    row.usage_count += u;
                }
            }
        }
    }
    order
        .into_iter()
        .map(|k| {
            let r = rows.remove(&k).unwrap();
            json!({"token": r.token, "value": r.value, "category": r.category, "declared": r.declared, "usage_count": r.usage_count, "sources": r.sources})
        })
        .collect()
}

/// `export function extractTokensFromTree(tree, src, path)`
pub fn extractTokensFromTree(tree: Option<&Tree>, src: &str, path: &str) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(tree) = tree {
        out.extend(extractStyleTokens(Some(tree.root_node()), path, src.as_bytes(), None));
    }
    let chunks = crate::codebase_style_locate::locateStyles(&json!(src), &json!(path));
    for chunk in chunks.as_array().cloned().unwrap_or_default() {
        let css = str_qq(chunk.get("css"));
        out.extend(extractCssTokens(&css, path));
    }
    out
}

fn relative_path(root: &str, file: &Path) -> String {
    let root_path = Path::new(root);
    let rel = file.strip_prefix(root_path).unwrap_or(file);
    rel.to_string_lossy().replace('\\', "/")
}

/// `export function finalizeTokens(db, origin, findings, projectRoot)`
pub fn finalizeTokens(db: &Connection, origin: &str, findings: Vec<Value>, project_root: Option<&str>) -> Value {
    let mut all = findings;
    let mut style_files: i64 = 0;
    if let Some(root) = project_root.filter(|r| !r.is_empty()) {
        for file in walk_style_files(root) {
            let src = match std::fs::read_to_string(&file) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if src.is_empty() {
                continue;
            }
            let rel = relative_path(root, &file);
            let chunks = crate::codebase_style_locate::locateStyles(&json!(src), &json!(rel));
            for chunk in chunks.as_array().cloned().unwrap_or_default() {
                let css = str_qq(chunk.get("css"));
                all.extend(extractCssTokens(&css, &rel));
            }
            style_files += 1;
        }
    }
    let rows = accumulate(&all);
    let mut persisted = false;
    if crate::db_json::run_write(db, "DELETE FROM code_design_tokens WHERE origin = ?", &[json!(origin)]).is_ok() {
        persisted = true;
    }
    if persisted {
        for r in &rows {
            let sources_json = serde_json::to_string(r.get("sources").unwrap_or(&json!([]))).unwrap_or_else(|_| "[]".into());
            let _ = crate::db_json::run_write(
                db,
                "INSERT INTO code_design_tokens (origin, token, value, category, declared, usage_count, sources_json) VALUES (?,?,?,?,?,?,?)",
                &[
                    json!(origin),
                    r.get("token").cloned().unwrap_or(Value::Null),
                    r.get("value").cloned().unwrap_or(Value::Null),
                    r.get("category").cloned().unwrap_or(Value::Null),
                    r.get("declared").cloned().unwrap_or(Value::Null),
                    r.get("usage_count").cloned().unwrap_or(Value::Null),
                    json!(sources_json),
                ],
            );
        }
    }
    let declared = rows.iter().filter(|r| truthy(r.get("declared"))).count() as i64;
    let literals = rows.len() as i64 - declared;
    json!({"ok": true, "origin": origin, "style_files": style_files, "persisted": persisted, "tokens": rows.len() as i64, "declared": declared, "literals": literals})
}

/// `export default async function designTokensExtract(input, context)`
pub fn designTokensExtract(input: &Value, db: &Connection) -> Result<Value, String> {
    if input.get("source").filter(|v| !v.is_null()).is_some() {
        let path_val = input.get("path").and_then(Value::as_str);
        let grammar = grammarFor(path_val.unwrap_or(".tsx")).unwrap_or("tsx");
        let source_str = str_qq(input.get("source"));
        let tree = crate::codebase_parse_treesitter::parseToTree(&source_str, grammar);
        let root = tree.as_ref().map(|t| t.root_node());
        let path_for = path_val.unwrap_or("(source)");
        let theme_override = input.get("theme").map(|v| truthy(Some(v)));
        let findings = extractStyleTokens(root, path_for, source_str.as_bytes(), theme_override);
        return Ok(json!({"ok": true, "findings": findings}));
    }

    if truthy(input.get("all")) {
        let origin = input.get("origin").and_then(Value::as_str).unwrap_or("atlas").to_string();
        let rows = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin = ? AND type = 'file' ORDER BY path", &[json!(origin)])?;
        let files: Vec<String> = rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| r.get("path").and_then(Value::as_str).map(|s| s.to_string()))
            .filter(|p| grammarFor(p).is_some())
            .collect();

        let mut all: Vec<Value> = Vec::new();
        let mut parsed: i64 = 0;
        for f in &files {
            let content = match crate::codebase_codebase_file_read::codebaseFileRead(&json!({"path": f}), db) {
                Ok(v) => str_qq(v.get("content")),
                Err(_) => continue,
            };
            if content.is_empty() {
                continue;
            }
            let tree = grammarFor(f).and_then(|g| crate::codebase_parse_treesitter::parseToTree(&content, g));
            if let Some(t) = &tree {
                all.extend(extractStyleTokens(Some(t.root_node()), f, content.as_bytes(), None));
            }
            let chunks = crate::codebase_style_locate::locateStyles(&json!(content), &json!(f));
            for chunk in chunks.as_array().cloned().unwrap_or_default() {
                let css = str_qq(chunk.get("css"));
                all.extend(extractCssTokens(&css, f));
            }
            parsed += 1;
        }

        let project_root = crate::db_json::query_rows(db, "SELECT root FROM code_onboard_jobs ORDER BY rowid DESC LIMIT 1", &[])
            .ok()
            .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
            .and_then(|r| r.get("root").and_then(Value::as_str).map(|s| s.to_string()));

        let fin = finalizeTokens(db, &origin, all, project_root.as_deref());
        let mut obj = fin.as_object().cloned().unwrap_or_default();
        let style_files = obj.get("style_files").and_then(Value::as_i64).unwrap_or(0);
        obj.insert("files".into(), json!(parsed + style_files));
        return Ok(Value::Object(obj));
    }

    Err("design-tokens-extract: source|all is required".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-design-tokens-extract-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn classify_token_matches_category_priority() {
        assert_eq!(classifyToken("borderRadius", "8px"), "radius");
        assert_eq!(classifyToken("fontSize", "11px"), "font_size");
        assert_eq!(classifyToken("fontFamily", "Inter"), "font_family");
        assert_eq!(classifyToken("fontWeight", "600"), "font_weight");
        assert_eq!(classifyToken("zIndex", "10"), "z");
        assert_eq!(classifyToken("opacity", "0.5"), "opacity");
        assert_eq!(classifyToken("transition", "all .2s"), "motion");
        assert_eq!(classifyToken("color", "#fff"), "color");
        assert_eq!(classifyToken("border", "1px solid red"), "color");
        assert_eq!(classifyToken("brand", "oklch(0.5 0.1 200)"), "color");
        assert_eq!(classifyToken("padding", "13"), "spacing");
        assert_eq!(classifyToken("mono", "ui-monospace"), "font_family");
        assert_eq!(classifyToken("display", "flex"), "other");
    }

    #[test]
    fn grammar_for_maps_extensions() {
        assert_eq!(grammarFor("a.tsx"), Some("tsx"));
        assert_eq!(grammarFor("a.jsx"), Some("tsx"));
        assert_eq!(grammarFor("a.ts"), Some("typescript"));
        assert_eq!(grammarFor("a.css"), None);
    }

    #[test]
    fn extract_style_tokens_literals_and_theme_object() {
        let src = "const C = { brand: \"oklch(0.5 0.1 200)\", pad: 13 } as const;\nfunction Foo() { return <div style={{ color: \"#fff\", padding: 13, display: \"flex\" }}><Bar style={{ color: C.brand }}/></div>; }";
        let tree = crate::codebase_parse_treesitter::parseToTree(src, "tsx").unwrap();
        let findings = extractStyleTokens(Some(tree.root_node()), "theme.ts", src.as_bytes(), None);

        let declared: Vec<&Value> = findings.iter().filter(|f| truthy(f.get("declared"))).collect();
        assert!(declared.iter().any(|f| f["token"] == json!("C.brand") && f["category"] == json!("color")));
        // classifyToken("pad", ...) matches none of the padding/margin/... patterns (key is "pad", not
        // "padding") — real JS (verified via node repro) classifies it "other", not "spacing".
        assert!(declared.iter().any(|f| f["token"] == json!("C.pad") && f["category"] == json!("other")));

        let literals: Vec<&Value> = findings.iter().filter(|f| !truthy(f.get("declared")) && !truthy(f.get("isUsage"))).collect();
        assert!(literals.iter().any(|f| f["value"] == json!("#fff") && f["category"] == json!("color")));
        assert!(literals.iter().any(|f| f["value"] == json!("13") && f["category"] == json!("spacing")));
        // display:flex classifies "other" and is dropped
        assert!(!literals.iter().any(|f| f["value"] == json!("flex")));

        let usages: Vec<&Value> = findings.iter().filter(|f| truthy(f.get("isUsage"))).collect();
        assert!(usages.iter().any(|f| f["token"] == json!("C.brand")));
    }

    #[test]
    fn extract_style_tokens_non_theme_file_skips_declared() {
        let src = "const C = { brand: \"#fff\" };";
        let tree = crate::codebase_parse_treesitter::parseToTree(src, "tsx").unwrap();
        let findings = extractStyleTokens(Some(tree.root_node()), "component.tsx", src.as_bytes(), None);
        assert!(findings.is_empty());
    }

    #[test]
    fn extract_css_vars_reads_root_block() {
        let css = ":root {\n  --c-bg: oklch(0.2 0 0);\n  --gap: 8px;\n}\n.other { color: red; }";
        let out = extractCssVars(css, "styles.css");
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|r| r["token"] == json!("--c-bg") && r["category"] == json!("color")));
        assert!(out.iter().any(|r| r["token"] == json!("--gap") && r["category"] == json!("spacing")));
    }

    #[test]
    fn extract_css_tokens_scss_vars_props_and_literals() {
        let scss = "// a comment\n$brand: #ff00ff;\n$ref: $brand;\n:root { --c-bg: var(--other); }\n.c-blue { color: #5697fa; font-size: 1rem; display: flex; box-shadow: 0 1px 2px, 0 2px 4px; }";
        let out = extractCssTokens(scss, "a.scss");
        assert!(out.iter().any(|r| r["token"] == json!("$brand") && r["value"] == json!("#ff00ff") && r["declared"] == json!(1)));
        // $ref references another var — skipped
        assert!(!out.iter().any(|r| r["token"] == json!("$ref")));
        // --c-bg references var(...) — skipped as declared
        assert!(!out.iter().any(|r| r["token"] == json!("--c-bg")));
        assert!(out.iter().any(|r| r["value"] == json!("#5697fa") && r["category"] == json!("color") && r["declared"] == json!(0)));
        assert!(out.iter().any(|r| r["value"] == json!("1rem") && r["category"] == json!("font_size")));
        // display:flex → "other" → dropped
        assert!(!out.iter().any(|r| r["value"] == json!("flex")));
        // box-shadow is compound (comma outside parens) → dropped
        assert!(!out.iter().any(|r| r["value"].as_str().map(|s| s.contains("1px 2px")).unwrap_or(false)));
    }

    #[test]
    fn extract_css_tokens_border_shorthand_color_with_space_is_dropped() {
        let css = ".x { border: 1px solid red; }";
        let out = extractCssTokens(css, "a.css");
        assert!(out.is_empty());
    }

    #[test]
    fn accumulate_dedupes_and_credits_token_usage() {
        let findings = vec![
            json!({"token": "C.brand", "value": "#fff", "category": "color", "declared": 1, "usage": 0, "line": 1, "path": "theme.ts"}),
            json!({"token": null, "value": "#fff", "category": "color", "declared": 0, "usage": 1, "line": 5, "path": "a.tsx"}),
            json!({"token": null, "value": "#fff", "category": "color", "declared": 0, "usage": 1, "line": 9, "path": "b.tsx"}),
            json!({"token": "C.brand", "value": null, "category": "color", "isUsage": true}),
            json!({"token": "C.brand", "value": null, "category": "color", "isUsage": true}),
        ];
        let rows = accumulate(&findings);
        let declared_row = rows.iter().find(|r| r["token"] == json!("C.brand")).unwrap();
        assert_eq!(declared_row["usage_count"], json!(2));
        let literal_row = rows.iter().find(|r| r["token"].is_null()).unwrap();
        assert_eq!(literal_row["usage_count"], json!(2));
        assert_eq!(literal_row["sources"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn extract_tokens_from_tree_combines_cst_and_css_in_js() {
        let src = "const B = styled.button`color: green; padding: 4px;`;\nfunction Foo() { return <div style={{ color: \"#111\" }}/>; }";
        let tree = crate::codebase_parse_treesitter::parseToTree(src, "tsx");
        let out = extractTokensFromTree(tree.as_ref(), src, "a.tsx");
        assert!(out.iter().any(|f| f["value"] == json!("#111")));
        assert!(out.iter().any(|f| f["value"] == json!("green")));
        assert!(out.iter().any(|f| f["value"] == json!("4px")));
    }

    #[test]
    fn finalize_tokens_walks_project_root_and_persists() {
        let dir = temp_dir("finalize");
        fs::write(dir.join("styles.css"), ":root { --c-bg: #101010; }\n.a { color: #202020; }").unwrap();
        let sub = dir.join("node_modules");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("skip.css"), ".x { color: #ffffff; }").unwrap();

        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_design_tokens (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, token TEXT, value TEXT, category TEXT, declared INTEGER, usage_count INTEGER, sources_json TEXT)",
        )
        .unwrap();

        let out = finalizeTokens(&c, "atlas", Vec::new(), Some(dir.to_str().unwrap()));
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["persisted"], json!(true));
        assert_eq!(out["style_files"], json!(1));
        assert!(out["tokens"].as_i64().unwrap() >= 2);

        let count: i64 = c.query_row("SELECT COUNT(*) FROM code_design_tokens WHERE origin = 'atlas'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, out["tokens"].as_i64().unwrap());
        // node_modules was skipped — its color never landed
        let has_skipped: i64 = c
            .query_row("SELECT COUNT(*) FROM code_design_tokens WHERE value = '#ffffff'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(has_skipped, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn design_tokens_extract_source_mode_no_db_needed() {
        let c = Connection::open_in_memory().unwrap();
        let src = "function Foo() { return <div style={{ color: \"#abc\" }}/>; }";
        let out = designTokensExtract(&json!({"source": src, "path": "a.tsx"}), &c).unwrap();
        assert_eq!(out["ok"], json!(true));
        assert!(out["findings"].as_array().unwrap().iter().any(|f| f["value"] == json!("#abc")));
    }

    #[test]
    fn design_tokens_extract_requires_source_or_all() {
        let c = Connection::open_in_memory().unwrap();
        let e = designTokensExtract(&json!({}), &c).unwrap_err();
        assert_eq!(e, "design-tokens-extract: source|all is required");
    }

    #[test]
    fn design_tokens_extract_all_mode_sweeps_and_persists() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL);
             CREATE TABLE code_design_tokens (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, token TEXT, value TEXT, category TEXT, declared INTEGER, usage_count INTEGER, sources_json TEXT);
             CREATE TABLE code_onboard_jobs (id INTEGER PRIMARY KEY AUTOINCREMENT, root TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, root_path TEXT, is_active INTEGER);",
        )
        .unwrap();
        c.execute("INSERT INTO code_nodes (origin, path, type) VALUES ('atlas', 'a.tsx', 'file')", []).unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_nodes (origin, path, type) VALUES ('atlas', 'a.css', 'file')", &[]).ok();

        // codebaseFileRead reads from disk via a seeded workspace root; write the real file there.
        let dir = temp_dir("all-mode");
        fs::write(dir.join("a.tsx"), "function Foo() { return <div style={{ color: \"#654321\" }}/>; }").unwrap();
        crate::db_json::run_write(
            &c,
            "INSERT INTO workspaces (root_path, is_active) VALUES (?, 1)",
            &[json!(dir.to_str().unwrap())],
        )
        .unwrap();

        let out = designTokensExtract(&json!({"all": true, "origin": "atlas"}), &c).unwrap();
        assert_eq!(out["ok"], json!(true));
        let count: i64 = c.query_row("SELECT COUNT(*) FROM code_design_tokens WHERE origin = 'atlas'", [], |r| r.get(0)).unwrap();
        assert_eq!(count, out["tokens"].as_i64().unwrap());
        assert!(count >= 1);

        let _ = fs::remove_dir_all(&dir);
    }
}
