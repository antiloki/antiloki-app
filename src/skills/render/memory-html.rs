//! memory-html.rs — twin of skills/render/memory-html.mjs (db READ lane, cross-db: memory.memories).
//! NAMES ARE CONTRACT: `memoryHtml` (default). Single-page HTML view of memories, grouped by type.
// The template literals are CRLF in source but JS normalizes template-literal line terminators to
// LF, so the byte-exact output uses "\n" only (confirmed by running the JS and JSON.stringify-ing it).

use rusqlite::Connection;
use serde_json::Value;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `${v}` interpolation for present values (numbers/strings/bools).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS String(v) — used inside escapeHtml(v), which does String(v) before replacing.
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
// JS escapeHtml: & first, then <, >, " (order matters so produced entities aren't re-escaped).
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn escape_val(v: &Value) -> String {
    escape_html(&js_string(v))
}

const TYPE_COLORS: [(&str, &str); 5] = [
    ("lesson", "#4caf50"),
    ("preference", "#5b9bd5"),
    ("decision", "#e4b948"),
    ("reference", "#88ddff"),
    ("correction", "#ff6b6b"),
];
fn type_color(t: &str) -> &'static str {
    TYPE_COLORS.iter().find(|(k, _)| *k == t).map(|(_, v)| *v).unwrap_or("#88ddff")
}

// CSS constant — byte-exact value of the JS `const CSS` template literal (leading + trailing "\n").
const CSS: &str = "\n:root { --bg:#1e1e1e; --fg:#ddd; --muted:#888; --bar:#444; --card:#2a2a2a; }\n* { box-sizing: border-box; }\nbody { font-family: 'JetBrains Mono', Consolas, monospace; padding: 1.5em; background: var(--bg); color: var(--fg); max-width: 1100px; margin: 0 auto; }\nh1 { color: #fff; font-weight: 400; margin: 0 0 0.5em 0; }\nh2 { color: #fff; font-weight: 400; border-bottom: 1px solid var(--bar); padding-bottom: 0.3em; margin-top: 2em; }\n.type-badge { display: inline-block; padding: 1px 8px; border-radius: 3px; color: #000; font-weight: 600; font-size: 0.8em; text-transform: uppercase; }\n.entry { background: var(--card); padding: 1em; margin: 0.5em 0; border-radius: 4px; border-left: 3px solid; }\n.entry h3 { margin: 0 0 0.3em 0; color: #fff; font-weight: 500; }\n.entry .id { color: var(--muted); font-size: 0.85em; }\n.entry .conf { color: var(--muted); font-size: 0.85em; margin-left: 0.5em; }\n.entry .valid { color: #4caf50; }\n.entry .invalid { color: var(--muted); }\n.entry .row { margin: 0.4em 0; }\n.entry .label { color: var(--muted); display: inline-block; min-width: 70px; }\n.entry .naive { background: #3a1a1a; padding: 0.5em; border-radius: 3px; color: #ff9b9b; margin-top: 0.3em; }\n.tags { margin-top: 0.4em; }\n.tag { display: inline-block; background: #3a3a3a; color: var(--fg); padding: 1px 8px; border-radius: 10px; font-size: 0.8em; margin-right: 0.3em; }\n.empty { color: var(--muted); font-style: italic; }\n";

// The full <body><nav>…</nav> line, verbatim (contains a source typo `<body>quot;` for `&quot;`).
const NAV: &str = r#"<body><nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>"#;

// JS `JSON.parse(m.tags_json || '[]') || []`.
fn parse_tags(tags_json: Option<&Value>) -> Vec<Value> {
    let raw: &str = match tags_json {
        Some(Value::String(s)) if !s.is_empty() => s.as_str(),
        _ => "[]",
    };
    match serde_json::from_str::<Value>(raw).unwrap_or(Value::Null) {
        Value::Array(a) => a,
        _ => Vec::new(),
    }
}

fn render_item(m: &Value, color: &str) -> String {
    let id = tmpl(m.get("id"));
    let title = escape_val(m.get("title").unwrap_or(&Value::Null));
    let situation = escape_val(m.get("situation").unwrap_or(&Value::Null));
    let guidance = escape_val(m.get("guidance").unwrap_or(&Value::Null));
    let valid = truthy(m.get("validated"));
    let valid_class = if valid { "valid" } else { "invalid" };
    let valid_mark = if valid { "\u{2713}" } else { "\u{b7}" };
    let conf = m.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let tags_html: String = parse_tags(m.get("tags_json"))
        .iter()
        .map(|t| format!("<span class=\"tag\">{}</span>", escape_val(t)))
        .collect::<Vec<_>>()
        .join("");
    let naive_html = if truthy(m.get("naive_response")) {
        format!(
            "<div class=\"row\"><span class=\"label\">naive:</span><div class=\"naive\">{}</div></div>",
            escape_val(m.get("naive_response").unwrap())
        )
    } else {
        String::new()
    };
    let rationale_html = if truthy(m.get("rationale")) {
        format!("<div class=\"row\"><span class=\"label\">why:</span> {}</div>", escape_val(m.get("rationale").unwrap()))
    } else {
        String::new()
    };

    format!(
        "<article class=\"entry\" id=\"m-{id}\" style=\"border-left-color:{color}\">\n  <h3>\n    <span class=\"id\">#{id}</span>\n    <span class=\"{valid_class}\">{valid_mark}</span>\n    <span class=\"conf\">[{conf:.2}]</span>\n    {title}\n  </h3>\n  <div class=\"row\"><span class=\"label\">when:</span> {situation}</div>\n  <div class=\"row\"><span class=\"label\">do:</span> {guidance}</div>\n  {rationale_html}\n  {naive_html}\n  <div class=\"tags\">{tags_html}</div>\n</article>"
    )
}

pub fn memoryHtml(_input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-html','db') when no db — the db lane always injects one.
    let rows = crate::db_json::query_rows(db, "SELECT * FROM memory.memories WHERE superseded_by IS NULL ORDER BY type, id DESC", &[])?
        .as_array()
        .cloned()
        .unwrap_or_default();

    let mut order: Vec<String> = Vec::new();
    let mut grouped: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for r in &rows {
        let t = r.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !grouped.contains_key(&t) {
            order.push(t.clone());
            grouped.insert(t.clone(), Vec::new());
        }
        grouped.get_mut(&t).unwrap().push(r.clone());
    }

    let mut sections: Vec<String> = Vec::new();
    if rows.is_empty() {
        sections.push(r#"<p class="empty">No memories yet.</p>"#.to_string());
    } else {
        for t in &order {
            let group = &grouped[t];
            let color = type_color(t);
            let items: Vec<String> = group.iter().map(|m| render_item(m, color)).collect();
            sections.push(format!(
                "<h2><span class=\"type-badge\" style=\"background:{color}\">{}</span> <span style=\"color:#888;font-size:0.7em\">({})</span></h2>\n{}",
                escape_html(t),
                group.len(),
                items.join("\n")
            ));
        }
    }

    Ok(Value::String(format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Memory · antiloki</title>\n<style>{CSS}</style>\n</head>\n{NAV}\n<h1>Memory · {} active</h1>\n{}\n</body>\n</html>",
        rows.len(),
        sections.join("\n")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SCHEMA: &str = "CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, situation TEXT NOT NULL, guidance TEXT NOT NULL, naive_response TEXT, rationale TEXT, tags_json TEXT NOT NULL DEFAULT '[]', confidence REAL NOT NULL DEFAULT 0.5, validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, created_by TEXT NOT NULL);";

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn empty_renders_no_memories_message() {
        let c = conn();
        let got = memoryHtml(&json!({}), &c).unwrap();
        let s = got.as_str().unwrap();
        assert!(s.starts_with("<!DOCTYPE html>\n<html lang=\"en\">\n"));
        assert!(s.contains("<title>Memory · antiloki</title>"));
        assert!(s.contains("<h1>Memory · 0 active</h1>\n<p class=\"empty\">No memories yet.</p>\n</body>\n</html>"));
    }

    #[test]
    fn groups_by_type_orders_within_group_desc_and_excludes_superseded() {
        let c = conn();
        c.execute_batch(
            "INSERT INTO memory.memories (id,type,title,situation,guidance,naive_response,rationale,tags_json,confidence,validated,superseded_by,created_by) VALUES
              (1,'lesson','Lesson <one>','sit1','gd1',NULL,NULL,'[\"<b>x</b>\",\"normal\"]',0.5,0,NULL,'test'),
              (2,'lesson','Lesson two','sit2','gd2',NULL,NULL,'[]',0.83,1,NULL,'test'),
              (3,'correction','C1','s3','g3','wrong <b>stuff</b>','why & how','[]',0.5,0,NULL,'test'),
              (4,'preference','Pref1','sit4','gd4',NULL,NULL,'[]',1,1,NULL,'test'),
              (5,'lesson','Superseded','sitS','gdS',NULL,NULL,'[]',0.2,0,2,'test'),
              (6,'decision','Dec1','sit6','gd6',NULL,NULL,'[]',0.5,0,NULL,'test'),
              (7,'reference','Ref1','sit7','gd7',NULL,NULL,'[]',0.6,1,NULL,'test'),
              (8,'narrative','Narr1','sit8','gd8',NULL,NULL,'[]',0.33,0,NULL,'test');",
        )
        .unwrap();
        let got = memoryHtml(&json!({}), &c).unwrap();
        let expected = "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Memory · antiloki</title>\n<style>\n:root { --bg:#1e1e1e; --fg:#ddd; --muted:#888; --bar:#444; --card:#2a2a2a; }\n* { box-sizing: border-box; }\nbody { font-family: 'JetBrains Mono', Consolas, monospace; padding: 1.5em; background: var(--bg); color: var(--fg); max-width: 1100px; margin: 0 auto; }\nh1 { color: #fff; font-weight: 400; margin: 0 0 0.5em 0; }\nh2 { color: #fff; font-weight: 400; border-bottom: 1px solid var(--bar); padding-bottom: 0.3em; margin-top: 2em; }\n.type-badge { display: inline-block; padding: 1px 8px; border-radius: 3px; color: #000; font-weight: 600; font-size: 0.8em; text-transform: uppercase; }\n.entry { background: var(--card); padding: 1em; margin: 0.5em 0; border-radius: 4px; border-left: 3px solid; }\n.entry h3 { margin: 0 0 0.3em 0; color: #fff; font-weight: 500; }\n.entry .id { color: var(--muted); font-size: 0.85em; }\n.entry .conf { color: var(--muted); font-size: 0.85em; margin-left: 0.5em; }\n.entry .valid { color: #4caf50; }\n.entry .invalid { color: var(--muted); }\n.entry .row { margin: 0.4em 0; }\n.entry .label { color: var(--muted); display: inline-block; min-width: 70px; }\n.entry .naive { background: #3a1a1a; padding: 0.5em; border-radius: 3px; color: #ff9b9b; margin-top: 0.3em; }\n.tags { margin-top: 0.4em; }\n.tag { display: inline-block; background: #3a3a3a; color: var(--fg); padding: 1px 8px; border-radius: 10px; font-size: 0.8em; margin-right: 0.3em; }\n.empty { color: var(--muted); font-style: italic; }\n</style>\n</head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"./index.html\" style=\"color:#58a6ff;text-decoration:none;\">← antiloki docs</a></nav>\n<h1>Memory · 7 active</h1>\n<h2><span class=\"type-badge\" style=\"background:#ff6b6b\">correction</span> <span style=\"color:#888;font-size:0.7em\">(1)</span></h2>\n<article class=\"entry\" id=\"m-3\" style=\"border-left-color:#ff6b6b\">\n  <h3>\n    <span class=\"id\">#3</span>\n    <span class=\"invalid\">·</span>\n    <span class=\"conf\">[0.50]</span>\n    C1\n  </h3>\n  <div class=\"row\"><span class=\"label\">when:</span> s3</div>\n  <div class=\"row\"><span class=\"label\">do:</span> g3</div>\n  <div class=\"row\"><span class=\"label\">why:</span> why &amp; how</div>\n  <div class=\"row\"><span class=\"label\">naive:</span><div class=\"naive\">wrong &lt;b&gt;stuff&lt;/b&gt;</div></div>\n  <div class=\"tags\"></div>\n</article>\n<h2><span class=\"type-badge\" style=\"background:#e4b948\">decision</span> <span style=\"color:#888;font-size:0.7em\">(1)</span></h2>\n<article class=\"entry\" id=\"m-6\" style=\"border-left-color:#e4b948\">\n  <h3>\n    <span class=\"id\">#6</span>\n    <span class=\"invalid\">·</span>\n    <span class=\"conf\">[0.50]</span>\n    Dec1\n  </h3>\n  <div class=\"row\"><span class=\"label\">when:</span> sit6</div>\n  <div class=\"row\"><span class=\"label\">do:</span> gd6</div>\n  \n  \n  <div class=\"tags\"></div>\n</article>\n<h2><span class=\"type-badge\" style=\"background:#4caf50\">lesson</span> <span style=\"color:#888;font-size:0.7em\">(2)</span></h2>\n<article class=\"entry\" id=\"m-2\" style=\"border-left-color:#4caf50\">\n  <h3>\n    <span class=\"id\">#2</span>\n    <span class=\"valid\">✓</span>\n    <span class=\"conf\">[0.83]</span>\n    Lesson two\n  </h3>\n  <div class=\"row\"><span class=\"label\">when:</span> sit2</div>\n  <div class=\"row\"><span class=\"label\">do:</span> gd2</div>\n  \n  \n  <div class=\"tags\"></div>\n</article>\n<article class=\"entry\" id=\"m-1\" style=\"border-left-color:#4caf50\">\n  <h3>\n    <span class=\"id\">#1</span>\n    <span class=\"invalid\">·</span>\n    <span class=\"conf\">[0.50]</span>\n    Lesson &lt;one&gt;\n  </h3>\n  <div class=\"row\"><span class=\"label\">when:</span> sit1</div>\n  <div class=\"row\"><span class=\"label\">do:</span> gd1</div>\n  \n  \n  <div class=\"tags\"><span class=\"tag\">&lt;b&gt;x&lt;/b&gt;</span><span class=\"tag\">normal</span></div>\n</article>\n<h2><span class=\"type-badge\" style=\"background:#88ddff\">narrative</span> <span style=\"color:#888;font-size:0.7em\">(1)</span></h2>\n<article class=\"entry\" id=\"m-8\" style=\"border-left-color:#88ddff\">\n  <h3>\n    <span class=\"id\">#8</span>\n    <span class=\"invalid\">·</span>\n    <span class=\"conf\">[0.33]</span>\n    Narr1\n  </h3>\n  <div class=\"row\"><span class=\"label\">when:</span> sit8</div>\n  <div class=\"row\"><span class=\"label\">do:</span> gd8</div>\n  \n  \n  <div class=\"tags\"></div>\n</article>\n<h2><span class=\"type-badge\" style=\"background:#5b9bd5\">preference</span> <span style=\"color:#888;font-size:0.7em\">(1)</span></h2>\n<article class=\"entry\" id=\"m-4\" style=\"border-left-color:#5b9bd5\">\n  <h3>\n    <span class=\"id\">#4</span>\n    <span class=\"valid\">✓</span>\n    <span class=\"conf\">[1.00]</span>\n    Pref1\n  </h3>\n  <div class=\"row\"><span class=\"label\">when:</span> sit4</div>\n  <div class=\"row\"><span class=\"label\">do:</span> gd4</div>\n  \n  \n  <div class=\"tags\"></div>\n</article>\n<h2><span class=\"type-badge\" style=\"background:#88ddff\">reference</span> <span style=\"color:#888;font-size:0.7em\">(1)</span></h2>\n<article class=\"entry\" id=\"m-7\" style=\"border-left-color:#88ddff\">\n  <h3>\n    <span class=\"id\">#7</span>\n    <span class=\"valid\">✓</span>\n    <span class=\"conf\">[0.60]</span>\n    Ref1\n  </h3>\n  <div class=\"row\"><span class=\"label\">when:</span> sit7</div>\n  <div class=\"row\"><span class=\"label\">do:</span> gd7</div>\n  \n  \n  <div class=\"tags\"></div>\n</article>\n</body>\n</html>";
        assert_eq!(got, Value::String(expected.to_string()));
    }
}
