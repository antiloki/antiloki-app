//! learning-center-section.rs — twin of skills/docs/learning-center-section.mjs (standard, no db).
//! NAMES ARE CONTRACT: `learningCenterSection` (default) · `renderNav` · `escapeHtml`.
// QUALITY: 80/100 — pure formatter: renders one learning-center section page to a standalone HTML doc.
use rusqlite::Connection;
use serde_json::{json, Value};

// Nav ordering is part of the output — same order as the JS NAV_PAGES literal. Data const (not a unit).
const NAV_PAGES: [(&str, &str); 10] = [
    ("index", "home"),
    ("overview", "overview"),
    ("agents", "agents"),
    ("actions", "actions"),
    ("workflows", "workflows"),
    ("laws", "laws"),
    ("memory", "memory"),
    ("driver", "driver"),
    ("audit-chain", "audit chain"),
    ("dbs", "dbs"),
];

// Byte-for-byte the SHARED_CSS template literal (leading + trailing newline included).
const SHARED_CSS: &str = r#"
  :root { --bg:#0e1116; --panel:#161b22; --panel2:#1f2530;
          --border:#30363d; --fg:#e6edf3; --muted:#8b949e;
          --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; }
  * { box-sizing:border-box; }
  body { font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;
         background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }
  .wrap { max-width:1000px; margin:0 auto; padding:2em 1.5em 4em; }
  h1 { font-size:1.8em; margin:0 0 0.2em; }
  h2 { font-size:1.25em; margin:1.8em 0 0.5em; border-bottom:1px solid var(--border); padding-bottom:0.3em; }
  h3 { font-size:1em; margin:1.3em 0 0.4em; color:var(--accent); }
  p  { margin:0.5em 0; }
  a  { color:var(--accent); text-decoration:none; }
  a:hover { text-decoration:underline; }
  code { background:var(--panel); padding:0.1em 0.3em; border-radius:3px;
         font-family:"SF Mono", Consolas, monospace; font-size:0.9em; }
  pre { background:var(--panel); padding:0.8em 1em; border-radius:6px;
        overflow-x:auto; border:1px solid var(--border); }
  pre code { background:transparent; padding:0; }
  table { width:100%; border-collapse:collapse; margin:0.8em 0; font-size:0.95em; }
  th, td { padding:0.5em 0.7em; text-align:left; border-bottom:1px solid var(--border); }
  th { color:var(--muted); font-weight:600; font-size:0.85em; text-transform:uppercase; letter-spacing:0.05em; }
  tr:hover { background:var(--panel); }
  .muted { color:var(--muted); }
  .summary { background:var(--panel); padding:1em 1.2em; border-left:3px solid var(--accent);
             border-radius:0 6px 6px 0; margin:1em 0 2em; }
  .badge { display:inline-block; background:var(--panel2); padding:0.1em 0.5em;
           border-radius:10px; font-size:0.8em; color:var(--muted); }
  nav.topbar { background:var(--panel); border-bottom:1px solid var(--border);
               padding:0.6em 1em; display:flex; gap:1em; flex-wrap:wrap; }
  nav.topbar a { color:var(--muted); font-size:0.9em; }
  nav.topbar a.active { color:var(--accent); }
  footer { margin-top:3em; padding-top:1em; border-top:1px solid var(--border);
           color:var(--muted); font-size:0.85em; }
"#;

// JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `${v}`
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// String(s ?? '').replace(/[&<>"']/g, ...) — single pass, no re-scan of substitutions.
fn escape_html_str(s: Option<&Value>) -> String {
    let src = str_qq(s);
    let mut out = String::with_capacity(src.len());
    for c in src.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn render_nav_str(current_slug: &Value) -> String {
    let cur = current_slug.as_str(); // strict === against string slug: non-string never matches
    let mut s = String::from("<nav class=\"topbar\">");
    for (slug, title) in NAV_PAGES {
        s.push_str("<a href=\"./");
        s.push_str(slug);
        s.push_str(".html\"");
        if cur == Some(slug) {
            s.push_str(" class=\"active\"");
        }
        s.push('>');
        s.push_str(title);
        s.push_str("</a>");
    }
    s.push_str("</nav>");
    s
}

// renderPage({ title, slug, summary_md, body_html, footer_html }) — key values already resolved.
fn render_page(title: Option<&Value>, slug: &Value, summary_md: &Value, body_html: &Value, footer_html: &Value) -> String {
    let title_esc = escape_html_str(title);
    let nav = render_nav_str(slug);
    let summary_section = if truthy(Some(summary_md)) {
        format!("<div class=\"summary\">{}</div>", escape_html_str(Some(summary_md)))
    } else {
        String::new()
    };
    let body = tmpl(Some(body_html));
    let footer = tmpl(Some(footer_html));
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"UTF-8\"><title>{title} \u{b7} antiloki learning center</title>\n<style>{css}</style></head>\n<body>\n{nav}\n<div class=\"wrap\">\n  <h1>{title}</h1>\n  {summary}\n  {body}\n  <footer>\n    Generated by <code>system.learning_center.render</code>.\n    {footer}\n  </footer>\n</div>\n</body></html>",
        title = title_esc,
        css = SHARED_CSS,
        nav = nav,
        summary = summary_section,
        body = body,
        footer = footer,
    )
}

pub fn renderNav(current_slug: &Value) -> Value {
    json!(render_nav_str(current_slug))
}

pub fn escapeHtml(s: &Value) -> Value {
    json!(escape_html_str(Some(s)))
}

pub fn learningCenterSection(input: &Value, _db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let field = |k: &str| obj.and_then(|o| o.get(k));
    let title = field("title");
    let slug = field("slug");
    // destructuring defaults apply only when the value is undefined (absent) — null passes through.
    let summary_md = field("summary_md").cloned().unwrap_or_else(|| json!(""));
    let body_html = field("body_html").cloned().unwrap_or_else(|| json!(""));
    let footer_html = field("footer_html").cloned().unwrap_or_else(|| json!(""));
    if !truthy(title) {
        return Err("learning-center-section: title is required".into());
    }
    if !truthy(slug) {
        return Err("learning-center-section: slug is required".into());
    }
    let slug_val = slug.cloned().unwrap_or(Value::Null);
    Ok(json!(render_page(title, &slug_val, &summary_md, &body_html, &footer_html)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection { Connection::open_in_memory().unwrap() }

    #[test]
    fn escapes() {
        assert_eq!(escapeHtml(&json!("<a>&\"'</a>")), json!("&lt;a&gt;&amp;&quot;&#39;&lt;/a&gt;"));
        assert_eq!(escapeHtml(&json!("plain")), json!("plain"));
        assert_eq!(escapeHtml(&Value::Null), json!(""));
        assert_eq!(escapeHtml(&json!(5)), json!("5"));
    }

    #[test]
    fn nav() {
        let idx = renderNav(&json!("index"));
        let s = idx.as_str().unwrap();
        assert!(s.starts_with("<nav class=\"topbar\"><a href=\"./index.html\" class=\"active\">home</a>"));
        assert!(s.contains("<a href=\"./audit-chain.html\">audit chain</a>"));
        assert!(s.ends_with("<a href=\"./dbs.html\">dbs</a></nav>"));
        // non-matching slug → no active anywhere
        assert!(!renderNav(&json!("nope")).as_str().unwrap().contains("class=\"active\""));
        // non-string current_slug → no active
        assert!(!renderNav(&Value::Null).as_str().unwrap().contains("class=\"active\""));
    }

    #[test]
    fn page_full_and_min() {
        let full = learningCenterSection(
            &json!({ "title": "Overview <x>", "slug": "overview", "summary_md": "sum & <b>", "body_html": "<p>body</p>", "footer_html": "<span>f</span>" }),
            &db(),
        ).unwrap();
        let f = full.as_str().unwrap();
        assert!(f.starts_with("<!DOCTYPE html>\n<html lang=\"en\"><head>"));
        assert!(f.contains("<title>Overview &lt;x&gt; \u{b7} antiloki learning center</title>"));
        assert!(f.contains("<h1>Overview &lt;x&gt;</h1>"));
        assert!(f.contains("<div class=\"summary\">sum &amp; &lt;b&gt;</div>"));
        assert!(f.contains("<a href=\"./overview.html\" class=\"active\">overview</a>"));
        assert!(f.contains("\n  <p>body</p>\n  <footer>"));
        assert!(f.contains("system.learning_center.render</code>.\n    <span>f</span>\n  </footer>"));

        let min = learningCenterSection(&json!({ "title": "Home", "slug": "index" }), &db()).unwrap();
        let m = min.as_str().unwrap();
        assert!(m.contains("<title>Home \u{b7} antiloki learning center</title>"));
        assert!(m.contains("<h1>Home</h1>\n  \n  \n  <footer>")); // empty summary + empty body
        assert!(!m.contains("class=\"summary\""));
        assert!(m.ends_with("</footer>\n</div>\n</body></html>"));

        // guards throw → Err (success path only is fixtured; this just proves the branch)
        assert!(learningCenterSection(&json!({ "slug": "x" }), &db()).is_err());
        assert!(learningCenterSection(&json!({ "title": "x" }), &db()).is_err());
    }
}
