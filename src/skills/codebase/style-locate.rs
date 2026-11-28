//! style-locate.rs — twin of skills/codebase/style-locate.mjs (db lane; db unused). NAMES ARE CONTRACT:
//! locateStyles (no-db) + styleLocate (default). Find CSS-shaped text across frameworks (LOCATE half).
// QUALITY: 79/100 — stylesheet ext, <style> blocks, CSS-in-JS tagged templates, Angular inline styles (regex)
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;
use regex::Regex;

fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), o => o.to_string() } }
fn ext_of(p: &str) -> String {
    static C: OnceLock<Regex> = OnceLock::new();
    let re = C.get_or_init(|| Regex::new(r"\.[^./\\]+$").unwrap());
    re.find(p).map(|m| m.as_str().to_lowercase()).unwrap_or_default()
}
const STYLESHEET_EXT: [&str; 5] = [".css", ".scss", ".sass", ".less", ".styl"];

struct Res { style: Regex, cssinjs: Regex, ng: Regex, ng_styles: Regex, ng_strs: Regex }
fn res() -> &'static Res {
    static C: OnceLock<Res> = OnceLock::new();
    C.get_or_init(|| Res {
        style: Regex::new(r"(?is)<style[^>]*>(.*?)</style>").unwrap(),
        cssinjs: Regex::new(r"(?s)\b(?:styled\.\w+|styled\([^)]*\)|css|createGlobalStyle|keyframes|injectGlobal)\s*`(.*?)`").unwrap(),
        ng: Regex::new(r"(?s)@Component\s*\(\s*\{.*?\}\s*\)").unwrap(),
        ng_styles: Regex::new(r"(?s)styles\s*:\s*\[(.*?)\]").unwrap(),
        ng_strs: Regex::new(r#"(?s)[`'"](.*?)[`'"]"#).unwrap(),
    })
}

pub fn locateStyles(source: &Value, path: &Value) -> Value {
    let src = js_str(source);
    let path_s = js_str(path);
    let mut out: Vec<Value> = Vec::new();

    if STYLESHEET_EXT.contains(&ext_of(&path_s).as_str()) {
        if !src.trim().is_empty() { out.push(json!({ "kind": "stylesheet", "css": src })); }
        return json!(out);
    }
    let r = res();
    for m in r.style.captures_iter(&src) {
        let body = m.get(1).map(|x| x.as_str()).unwrap_or("");
        if !body.trim().is_empty() { out.push(json!({ "kind": "style-block", "css": body })); }
    }
    for m in r.cssinjs.captures_iter(&src) {
        let body = m.get(1).map(|x| x.as_str()).unwrap_or("");
        if body.contains(':') { out.push(json!({ "kind": "css-in-js", "css": body })); }
    }
    if let Some(ng) = r.ng.find(&src) {
        let block = ng.as_str();
        for arr in r.ng_styles.captures_iter(block) {
            let inner = arr.get(1).map(|x| x.as_str()).unwrap_or("");
            for s in r.ng_strs.captures_iter(inner) {
                let body = s.get(1).map(|x| x.as_str()).unwrap_or("");
                if body.contains(':') { out.push(json!({ "kind": "angular-styles", "css": body })); }
            }
        }
    }
    json!(out)
}

pub fn styleLocate(input: &Value, _db: &Connection) -> Result<Value, String> {
    if input.get("source").map(|v| v.is_null()).unwrap_or(true) {
        return Err("style-locate: source is required".into());
    }
    let path = input.get("path").cloned().unwrap_or(json!(""));
    Ok(json!({ "ok": true, "chunks": locateStyles(input.get("source").unwrap(), &path) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locates() {
        assert_eq!(locateStyles(&json!(".a{color:red}"), &json!("x.scss")), json!([{ "kind": "stylesheet", "css": ".a{color:red}" }]));
        let vue = locateStyles(&json!("<template><div/></template>\n<style scoped>.a{color:blue}</style>"), &json!("Card.vue"));
        assert_eq!(vue.as_array().unwrap()[0]["kind"], json!("style-block"));
        let js = locateStyles(&json!("const B = styled.button`color: green;`;\nconst x = css`margin: 4px;`;"), &json!("b.tsx"));
        assert_eq!(js.as_array().unwrap().len(), 2);
        assert_eq!(locateStyles(&json!("const q = gql`query { a b }`;"), &json!("q.ts")).as_array().unwrap().len(), 0); // no ':'
        let c = Connection::open_in_memory().unwrap();
        assert!(styleLocate(&json!({}), &c).is_err());
    }
}
