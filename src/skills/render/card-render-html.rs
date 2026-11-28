//! card-render-html.rs — twin of skills/render/card-render-html.mjs (db READ lane). NAMES ARE
//! CONTRACT: `cardRenderHtml` (named default). One card + its linked contract chain + audit events.
// QUALITY: 80/100 — byte-exact HTML; escHtml, body JSON.parse/stringify roundtrip, ?? '-' cells
use rusqlite::Connection;
use serde_json::{json, Value};

// JS `String(v ?? '')`
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// sequential replace, same order as JS escHtml
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn esch(v: Option<&Value>) -> String { esc(&str_qq(v)) }

const CSS: &str = "\n:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; --rag:#bc8cff; }\nbody { font-family: -apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }\n.wrap { max-width:1080px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.7em; margin:0 0 0.2em; }\nh2 { font-size:1.25em; margin-top:2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; margin-bottom:0.2em; }\n.meta { color:var(--muted); font-size:0.85em; margin:0.3em 0 1em; font-family:ui-monospace,monospace; }\ncode,pre { font-family:ui-monospace,Consolas,monospace; font-size:0.88em; }\ncode { background:rgba(0,0,0,0.3); padding:1px 5px; border-radius:3px; border:1px solid var(--border); }\npre { background:#0d1117; padding:0.9em 1.1em; border-radius:6px; border:1px solid var(--border); overflow-x:auto; white-space:pre-wrap; }\ntable { width:100%; border-collapse:collapse; margin:0.5em 0 1.2em; font-size:0.92em; }\nth,td { padding:0.55em 0.8em; text-align:left; border-bottom:1px solid var(--border); vertical-align:top; }\nth { color:var(--muted); background:var(--panel); font-weight:500; font-size:0.85em; }\n.badges { display:flex; gap:0.5em; flex-wrap:wrap; margin:0.4em 0 1em; }\n.badge { display:inline-block; padding:3px 9px; border-radius:10px; font-size:0.8em; font-family:ui-monospace,monospace; font-weight:500; background:var(--panel); border:1px solid var(--border); }\n.badge.status-backlog { color:var(--muted); }\n.badge.status-doing { background:rgba(88,166,255,0.12); color:var(--accent); border-color:rgba(88,166,255,0.4); }\n.badge.status-done { background:rgba(63,185,80,0.15); color:var(--good); border-color:rgba(63,185,80,0.4); }\n.muted { color:var(--muted); }\nhr { border:0; border-top:1px solid var(--border); margin:2em 0; }\na { color: var(--accent); }\n";

const P1: &str = "<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>card \u{b7} ";
const P2: &str = "</title><style>";
const P3: &str = "</style></head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"../index.html\" style=\"color:#58a6ff;text-decoration:none;\">\u{2190} antiloki docs</a></nav><div class=\"wrap\">\n<div class=\"kicker\">card</div>\n<h1>";
const P4: &str = "</h1>\n<div class=\"meta\"><code>";
const P5: &str = "</code></div>\n<div class=\"badges\">\n  <span class=\"badge status-";
const P6: &str = "\">";
const P7: &str = "</span>\n</div>\n\n<h2>Body</h2>\n<pre>";
const P8: &str = "</pre>\n\n<h2>Linked contracts (";
const P9: &str = ")</h2>\n";
const P10: &str = "\n\n<h2>Audit events (sacred_timeline)</h2>\n";
const P11: &str = "\n\n<hr><p class=\"muted\" style=\"text-align:center\">Auto-generated \u{b7} regen: <code>node bin/antiloki.mjs card.render.html '{\"slug\":\"";
const P12: &str = "\"}'</code></p>\n</div></body></html>";

const NO_CONTRACTS: &str = "<p class=\"muted\">No contracts drafted for this card yet. Run <code>contract.draft.auto</code>.</p>";
const NO_EVENTS: &str = "<p class=\"muted\">No events recorded.</p>";
const CT_HEAD: &str = "<table><tr><th>slug</th><th>status</th><th>tier</th><th>attempt</th><th>signed_by</th></tr>";
const EV_HEAD: &str = "<table><tr><th>ts</th><th>actor</th><th>operation</th></tr>";

pub fn cardRenderHtml(input: &Value, db: &Connection) -> Result<Value, String> {
    // slug: typeof input === 'string' ? input : input?.slug
    let slug = if let Some(s) = input.as_str() {
        s.to_string()
    } else {
        input.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    if slug.is_empty() {
        return Err("card-render-html: slug is required".into());
    }

    let card = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[json!(slug)])?;
    let card = match card.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("card '{}' not found", slug)),
    };

    // linked contracts (try/catch -> empty on error)
    let contracts = crate::db_json::query_rows(db,
        "SELECT slug, status, tier, current_attempt, signed_by FROM contracts WHERE card_slug = ? ORDER BY current_attempt ASC",
        &[json!(slug)]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();

    // audit events (try/catch -> empty on error)
    let events = crate::db_json::query_rows(db,
        "SELECT ts, actor, operation FROM sacred_timeline WHERE entity_type = 'card' AND entity_id = ? ORDER BY id ASC",
        &[json!(slug)]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();

    // body: let body = card.body_json; try { body = JSON.parse(body) } catch { keep raw }
    let raw_body = card.get("body_json").cloned().unwrap_or(Value::Null);
    let parse_src: Option<String> = match &raw_body {
        Value::Null => Some("null".to_string()),        // String(null) = "null"
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let body: Value = match parse_src {
        Some(s) => serde_json::from_str::<Value>(&s).unwrap_or_else(|_| raw_body.clone()),
        None => raw_body.clone(),
    };
    let body_render = match &body {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    let body_html = esc(&body_render);

    // badge class: escHtml(card.status || 'backlog'); text: escHtml(card.status)
    let status_v = card.get("status");
    let status_class = if truthy(status_v) { status_v.cloned().unwrap() } else { json!("backlog") };

    let contracts_section = if contracts.is_empty() {
        NO_CONTRACTS.to_string()
    } else {
        let rows: String = contracts.iter().map(|c| {
            let tier = nz(c.get("tier")).cloned().unwrap_or_else(|| json!("-"));
            let signed = nz(c.get("signed_by")).cloned().unwrap_or_else(|| json!("-"));
            format!(
                "<tr><td><a href=\"../contracts/{slug}.html\"><code>{slug}</code></a></td><td><span class=\"badge\">{status}</span></td><td>{tier}</td><td>{attempt}</td><td><code>{signed}</code></td></tr>",
                slug = esch(c.get("slug")),
                status = esch(c.get("status")),
                tier = esch(Some(&tier)),
                attempt = tmpl(c.get("current_attempt")),
                signed = esch(Some(&signed)),
            )
        }).collect();
        format!("{}{}</table>", CT_HEAD, rows)
    };

    let events_section = if events.is_empty() {
        NO_EVENTS.to_string()
    } else {
        let rows: String = events.iter().map(|e| {
            format!(
                "<tr><td class=\"muted\">{ts}</td><td><code>{actor}</code></td><td><code>{op}</code></td></tr>",
                ts = esch(e.get("ts")),
                actor = esch(e.get("actor")),
                op = esch(e.get("operation")),
            )
        }).collect();
        format!("{}{}</table>", EV_HEAD, rows)
    };

    let slug_e = esch(card.get("slug"));
    let mut out = String::new();
    out.push_str(P1);
    out.push_str(&slug_e);
    out.push_str(P2);
    out.push_str(CSS);
    out.push_str(P3);
    out.push_str(&esch(card.get("title")));
    out.push_str(P4);
    out.push_str(&slug_e);
    out.push_str(P5);
    out.push_str(&esch(Some(&status_class)));
    out.push_str(P6);
    out.push_str(&esch(status_v));
    out.push_str(P7);
    out.push_str(&body_html);
    out.push_str(P8);
    out.push_str(&contracts.len().to_string());
    out.push_str(P9);
    out.push_str(&contracts_section);
    out.push_str(P10);
    out.push_str(&events_section);
    out.push_str(P11);
    out.push_str(&slug_e);
    out.push_str(P12);

    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, body_json TEXT); \
             CREATE TABLE contracts (slug TEXT, status TEXT, tier TEXT, current_attempt INTEGER, signed_by TEXT, card_slug TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, ts TEXT, actor TEXT, operation TEXT);"
        ).unwrap();
        c
    }

    #[test]
    fn full_render_object_body() {
        let c = conn();
        c.execute_batch(
            "INSERT INTO cards VALUES ('CARD-A','Title <A> & \"stuff\"','doing','{\"kind\":\"feat\",\"size\":\"M\",\"why\":\"because\"}'); \
             INSERT INTO contracts VALUES ('C-1','signed','high',1,'agent:qa','CARD-A'),('C-2','draft',NULL,2,NULL,'CARD-A'); \
             INSERT INTO sacred_timeline (entity_type,entity_id,ts,actor,operation) VALUES ('card','CARD-A','2026-01-01T00:00:00Z','human:dani','card.add'),('card','CARD-A','2026-01-02T00:00:00Z','agent:be','card.update');"
        ).unwrap();
        let r = cardRenderHtml(&json!("CARD-A"), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.starts_with("<!DOCTYPE html>\n<html><head>"));
        assert!(s.contains("<title>card \u{b7} CARD-A</title>"));
        assert!(s.contains("<body>quot;Segoe UI<body>quot;")); // verbatim source bug preserved
        assert!(s.contains("<h1>Title &lt;A&gt; &amp; &quot;stuff&quot;</h1>"));
        assert!(s.contains("<span class=\"badge status-doing\">doing</span>"));
        // object body pretty-printed then escaped
        assert!(s.contains("<pre>{\n  &quot;kind&quot;: &quot;feat&quot;,\n  &quot;size&quot;: &quot;M&quot;,\n  &quot;why&quot;: &quot;because&quot;\n}</pre>"));
        assert!(s.contains("<h2>Linked contracts (2)</h2>"));
        assert!(s.contains("../contracts/C-1.html")); // tier '-' fallback for NULL
        assert!(s.contains("<td>high</td><td>1</td><td><code>agent:qa</code></td>"));
        assert!(s.contains("<td>-</td><td>2</td><td><code>-</code></td>"));
        assert!(s.contains("<td class=\"muted\">2026-01-01T00:00:00Z</td><td><code>human:dani</code></td><td><code>card.add</code></td>"));
        assert!(s.ends_with("card.render.html '{\"slug\":\"CARD-A\"}'</code></p>\n</div></body></html>"));
    }

    #[test]
    fn raw_body_no_children_and_null_status() {
        let c = conn();
        c.execute_batch("INSERT INTO cards VALUES ('CARD-B','Plain','backlog','not json <raw>');").unwrap();
        let r = cardRenderHtml(&json!({ "slug": "CARD-B" }), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.contains("<pre>not json &lt;raw&gt;</pre>")); // parse fails -> raw kept
        assert!(s.contains("<span class=\"badge status-backlog\">backlog</span>"));
        assert!(s.contains(NO_CONTRACTS));
        assert!(s.contains(NO_EVENTS));

        let c2 = conn();
        c2.execute_batch("INSERT INTO cards VALUES ('CARD-C','Nully',NULL,NULL);").unwrap();
        let r2 = cardRenderHtml(&json!("CARD-C"), &c2).unwrap();
        let s2 = r2.as_str().unwrap();
        assert!(s2.contains("<span class=\"badge status-backlog\"></span>")); // status||'backlog' class, empty text
        assert!(s2.contains("<pre>null</pre>")); // JSON.parse(null) -> null -> "null"
    }

    #[test]
    fn missing_slug_errs() {
        let c = conn();
        assert!(cardRenderHtml(&json!({}), &c).is_err());
    }
}
