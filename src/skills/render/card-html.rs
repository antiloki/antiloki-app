//! card-html.rs — twin of skills/render/card-html.mjs (db READ lane). NAMES ARE CONTRACT:
//! `cardHtml` (default). Renders one card as a standalone HTML page: title, status badge,
//! body, and the card's sacred_timeline events LEFT JOINed to their git commit shas.
// The template literals are CRLF in source but JS normalizes line terminators to LF, so the
// output uses "\n". The `<body>quot;` in the nav is verbatim from source (a corrupted &quot;).
use rusqlite::Connection;
use serde_json::Value;

// CSS constant — byte-exact value of the JS `const CSS` template (leading + trailing "\n").
const CSS: &str = "\n:root {\n  --bg: #1e1e1e; --fg: #ddd; --muted: #888; --accent: #88ddff;\n  --bar: #444; --card-bg: #2a2a2a;\n}\n* { box-sizing: border-box; }\nbody { font-family: 'JetBrains Mono', Consolas, monospace; padding: 1.5em; background: var(--bg); color: var(--fg); line-height: 1.5; max-width: 1000px; margin: 0 auto; }\nh1 { color: #fff; font-weight: 400; margin: 0 0 0.5em 0; }\nh1 .slug { color: var(--accent); }\n.status { display: inline-block; padding: 2px 10px; border-radius: 3px; color: #000; font-weight: 600; font-size: 0.85em; text-transform: uppercase; }\n.meta { color: var(--muted); font-size: 0.85em; margin-bottom: 1em; }\n.body { background: var(--card-bg); padding: 1em; border-radius: 4px; margin-bottom: 2em; white-space: pre-wrap; overflow-x: auto; }\nh2 { color: #fff; font-weight: 400; border-bottom: 1px solid var(--bar); padding-bottom: 0.3em; }\n.event { margin: 0.5em 0; padding: 0.5em 1em; background: var(--card-bg); border-left: 3px solid var(--accent); border-radius: 0 4px 4px 0; }\n.event .ts { color: var(--muted); font-size: 0.85em; }\n.event .op { color: var(--accent); font-weight: 600; margin: 0 0.5em; }\n.event .actor { color: #c896ff; }\n.event .sha { color: var(--muted); font-size: 0.85em; margin-left: 0.5em; }\n.event .payload { display: block; color: var(--muted); font-size: 0.8em; margin-top: 0.3em; padding-left: 0.5em; border-left: 1px dashed var(--bar); }\n.empty { color: var(--muted); font-style: italic; }\n.notfound { color: #ff6b6b; padding: 1em; background: #2a1a1a; border-radius: 4px; }\n";

// The full <body><nav>…</nav> line, verbatim (line 105 of the source; also the L3 prefix of not-found).
const NAV: &str = r#"<body><nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>"#;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS String(v)
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
// String(property access): a missing key is `undefined` → "undefined"; else String(value).
fn s_of(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(x) => js_string(x),
    }
}
// JS escapeHtml: & first, then <, >, " (order matters so produced entities aren't re-escaped).
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn status_color(status: &str) -> &'static str {
    match status {
        "backlog" => "#888",
        "doing" => "#e4b948",
        "done" => "#4caf50",
        "archived" => "#555",
        _ => "#88ddff",
    }
}

pub fn cardHtml(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_val: Value = if input.is_string() { input.clone() } else { input.get("slug").cloned().unwrap_or(Value::Null) };
    if !truthy(Some(&slug_val)) {
        return Err("card-html: 'slug' is required".into());
    }
    let esc_slug = escape_html(&js_string(&slug_val));

    let cards = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[slug_val.clone()])?;
    let cards_arr = cards.as_array().cloned().unwrap_or_default();
    let card = cards_arr.first();

    if card.is_none() {
        let l2 = format!(r#"<html><head><meta charset="utf-8"><title>Card not found · {}</title><style>{}</style></head>"#, esc_slug, CSS);
        let l3 = format!(r#"{}<div class="notfound">Card '{}' not found.</div></body></html>"#, NAV, esc_slug);
        return Ok(Value::String(format!("<!DOCTYPE html>\n{}\n{}", l2, l3)));
    }
    let card = card.unwrap();

    let events = crate::db_json::query_rows(
        db,
        "SELECT t.*, c.git_sha, c.committed_at FROM sacred_timeline t LEFT JOIN timeline_commits c ON c.timeline_id = t.id WHERE t.entity_type = 'card' AND t.entity_id = ? ORDER BY t.id ASC",
        &[slug_val.clone()],
    )?;
    let events_arr = events.as_array().cloned().unwrap_or_default();

    let sc = status_color(&s_of(card.get("status")));

    let events_html = if events_arr.is_empty() {
        r#"<p class="empty">(no events)</p>"#.to_string()
    } else {
        events_arr
            .iter()
            .map(|e| {
                let sha = if truthy(e.get("git_sha")) {
                    let short: String = js_string(e.get("git_sha").unwrap()).chars().take(7).collect();
                    format!(r#"<span class="sha">{}</span>"#, escape_html(&short))
                } else {
                    String::new()
                };
                let payload = if truthy(e.get("payload")) {
                    format!(r#"<span class="payload">{}</span>"#, escape_html(&s_of(e.get("payload"))))
                } else {
                    String::new()
                };
                [
                    r#"<div class="event">"#.to_string(),
                    format!(r#"  <span class="ts">{}</span>"#, escape_html(&s_of(e.get("ts")))),
                    format!(r#"  <span class="op">{}</span>"#, escape_html(&s_of(e.get("operation")))),
                    format!(r#"  by <span class="actor">{}</span>"#, escape_html(&s_of(e.get("actor")))),
                    format!("  {}", sha),
                    format!("  {}", payload),
                    "</div>".to_string(),
                ]
                .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let body_html = if truthy(card.get("body_json")) {
        format!(r#"<div class="body">{}</div>"#, escape_html(&s_of(card.get("body_json"))))
    } else {
        r#"<p class="empty">(no body)</p>"#.to_string()
    };

    let lines: Vec<String> = vec![
        "<!DOCTYPE html>".to_string(),
        r#"<html lang="en">"#.to_string(),
        "<head>".to_string(),
        r#"<meta charset="utf-8">"#.to_string(),
        format!(r#"<title>Card · {}</title>"#, esc_slug),
        format!("<style>{}</style>", CSS),
        "</head>".to_string(),
        NAV.to_string(),
        format!(r#"<h1>Card · <span class="slug">{}</span></h1>"#, esc_slug),
        format!(r#"<div><span class="status" style="background:{}">{}</span></div>"#, sc, escape_html(&s_of(card.get("status")))),
        format!(r#"<p class="meta">{}<br>created: {} · updated: {}</p>"#, escape_html(&s_of(card.get("title"))), escape_html(&s_of(card.get("created_at"))), escape_html(&s_of(card.get("updated_at")))),
        "<h2>Body</h2>".to_string(),
        body_html,
        format!(r#"<h2>Events · {}</h2>"#, events_arr.len()),
        events_html,
        "</body>".to_string(),
        "</html>".to_string(),
    ];
    Ok(Value::String(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SCHEMA: &str = "CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, body_json TEXT, created_at TEXT, updated_at TEXT); \
         CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, ts TEXT, operation TEXT, actor TEXT, payload TEXT); \
         CREATE TABLE timeline_commits (timeline_id INTEGER, git_sha TEXT, committed_at TEXT);";
    fn s(v: &Value) -> String { v.as_str().unwrap().to_string() }

    #[test]
    fn not_found_is_byte_exact() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        let got = cardHtml(&json!({ "slug": "GHOST<x>" }), &c).unwrap();
        let expected = format!(
            "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>Card not found · GHOST&lt;x&gt;</title><style>{}</style></head>\n{}<div class=\"notfound\">Card 'GHOST&lt;x&gt;' not found.</div></body></html>",
            CSS, NAV
        );
        assert_eq!(got, Value::String(expected));
    }

    #[test]
    fn found_no_body_no_events() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch("INSERT INTO cards VALUES ('CH-1','My HTML card','backlog',NULL,'c1','u1');").unwrap();
        let got = s(&cardHtml(&json!({ "slug": "CH-1" }), &c).unwrap());
        assert!(got.starts_with("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n"));
        assert!(got.contains("<title>Card · CH-1</title>"));
        assert!(got.contains(r#"<div><span class="status" style="background:#888">backlog</span></div>"#));
        assert!(got.contains(r#"<p class="empty">(no body)</p>"#));
        assert!(got.contains("<h2>Events · 0</h2>\n<p class=\"empty\">(no events)</p>\n</body>\n</html>"));
    }

    #[test]
    fn found_with_body_and_events() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch(
            "INSERT INTO cards VALUES ('CH-9','My <b>Title</b>','doing','{\"kind\":\"feat\"}','2026-01-01 00:00:00','2026-01-02 00:00:00'); \
             INSERT INTO sacred_timeline VALUES (1,'card','CH-9','2026-01-01T00:00:00Z','card.add','human:dani','{\"a\":1}'); \
             INSERT INTO sacred_timeline VALUES (2,'card','CH-9','2026-01-02T00:00:00Z','card.update','agent:x',NULL); \
             INSERT INTO timeline_commits VALUES (1,'abcdef1234567','2026-01-01');",
        )
        .unwrap();
        // string input exercises the `typeof input === 'string'` slug path
        let got = s(&cardHtml(&json!("CH-9"), &c).unwrap());
        assert!(got.contains(r#"<div><span class="status" style="background:#e4b948">doing</span></div>"#));
        assert!(got.contains("My &lt;b&gt;Title&lt;/b&gt;<br>created: 2026-01-01 00:00:00 · updated: 2026-01-02 00:00:00"));
        assert!(got.contains(r#"<div class="body">{&quot;kind&quot;:&quot;feat&quot;}</div>"#));
        assert!(got.contains("<h2>Events · 2</h2>"));
        // event 1: git_sha sliced to 7 chars + payload present
        assert!(got.contains("  <span class=\"sha\">abcdef1</span>\n  <span class=\"payload\">{&quot;a&quot;:1}</span>\n</div>"));
        // event 2: no sha, no payload → two "  " lines before </div>
        assert!(got.contains("  by <span class=\"actor\">agent:x</span>\n  \n  \n</div>"));
    }
}
