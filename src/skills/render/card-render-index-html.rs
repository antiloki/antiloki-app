//! card-render-index-html.rs — twin of skills/render/card-render-index-html.mjs (db READ lane).
//! NAMES ARE CONTRACT: `cardRenderIndexHtml` (default). Board view of all cards as a static HTML page.
// QUALITY: byte-exact template — HTML-escape chain, status-class fallback, contract counts, created slice.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS String(v) (no ??) — used only for the non-string, non-null property-key edge.
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// escHtml's body: .replace chain in JS order (& first so later entities aren't double-escaped).
fn esc_str(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn esc_html(v: Option<&Value>) -> String { esc_str(&str_qq(v)) }
// JS object property-key coercion (obj[k]).
fn prop_key(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => js_string(o),
    }
}

// Static template prefix through `<div class="summary">` — generated verbatim from the JS output
// (CSS const + head + nav + kicker + h1 + meta). The literal `<body>quot;` in the nav font-family
// is a byte-for-byte quirk of the JS source and is preserved intentionally.
const PREFIX: &str = "<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>cards \u{b7} board</title><style>\n:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; }\nbody { font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }\n.wrap { max-width:1280px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.7em; margin:0 0 0.2em; }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; margin-bottom:0.2em; }\n.meta { color:var(--muted); font-size:0.85em; margin:0.3em 0 1em; font-family:ui-monospace,monospace; }\ntable { width:100%; border-collapse:collapse; margin:1em 0; font-size:0.92em; }\nth { background:var(--panel); padding:0.55em 0.8em; text-align:left; color:var(--muted); border-bottom:1px solid var(--border); font-size:0.85em; }\ntd { padding:0.5em 0.8em; border-bottom:1px solid var(--border); }\ntr:hover td { background:rgba(88,166,255,0.04); }\na { color:var(--accent); text-decoration:none; }\na:hover { text-decoration:underline; }\ncode { font-family:ui-monospace,monospace; font-size:0.88em; background:rgba(0,0,0,0.3); padding:1px 5px; border-radius:3px; }\n.badge { padding:2px 8px; border-radius:10px; font-size:0.78em; font-family:ui-monospace,monospace; background:var(--panel); border:1px solid var(--border); }\n.badge.status-backlog { color:var(--muted); }\n.badge.status-doing { background:rgba(88,166,255,0.12); color:var(--accent); border-color:rgba(88,166,255,0.4); }\n.badge.status-done { background:rgba(63,185,80,0.15); color:var(--good); border-color:rgba(63,185,80,0.4); }\n.summary { display:grid; grid-template-columns:repeat(auto-fit,minmax(160px,1fr)); gap:0.8em; margin:1em 0; }\n.tile { background:var(--panel); border:1px solid var(--border); padding:0.7em 1em; border-radius:6px; }\n.tile .label { color:var(--muted); font-size:0.78em; }\n.tile .num { font-size:1.5em; font-weight:600; color:var(--accent); }\n.empty { background:var(--panel); border:1px solid var(--border); border-radius:8px; padding:2em; text-align:center; color:var(--muted); margin:2em 0; }\n</style></head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"../index.html\" style=\"color:#58a6ff;text-decoration:none;\">\u{2190} antiloki docs</a></nav><div class=\"wrap\">\n<div class=\"kicker\">antiloki-v3 \u{b7} cards board</div>\n<h1>All cards</h1>\n<div class=\"meta\">Auto-generated \u{b7} regen: <code>node bin/antiloki.mjs card.render.index.html '{}' &gt; docs/cards/index.html</code></div>\n<div class=\"summary\">";

pub fn cardRenderIndexHtml(_input: &Value, db: &Connection) -> Result<Value, String> {
    let cards = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, created_at FROM cards ORDER BY status ASC, created_at DESC LIMIT 1000",
        &[],
    )?;
    let cards = cards.as_array().cloned().unwrap_or_default();

    // Contract-count per card — try/catch in JS (table may not exist) → swallow errors to empty.
    let mut contract_counts: HashMap<String, i64> = HashMap::new();
    if let Some(rows) = crate::db_json::query_rows(
        db,
        "SELECT card_slug, COUNT(*) AS c FROM contracts WHERE card_slug IS NOT NULL GROUP BY card_slug",
        &[],
    )
    .ok()
    .and_then(|r| r.as_array().cloned())
    {
        for r in &rows {
            contract_counts.insert(prop_key(r.get("card_slug")), r.get("c").and_then(|v| v.as_i64()).unwrap_or(0));
        }
    }

    // Status tallies (byStatus.backlog/doing/done || 0).
    let (mut n_backlog, mut n_doing, mut n_done) = (0usize, 0usize, 0usize);
    for c in &cards {
        match c.get("status").and_then(|v| v.as_str()) {
            Some("backlog") => n_backlog += 1,
            Some("doing") => n_doing += 1,
            Some("done") => n_done += 1,
            _ => {}
        }
    }

    let tiles: [(&str, usize); 5] = [
        ("total", cards.len()),
        ("backlog", n_backlog),
        ("doing", n_doing),
        ("done", n_done),
        ("with contracts", contract_counts.len()),
    ];
    let summary: String = tiles
        .iter()
        .map(|&(label, num)| {
            format!(
                "<div class=\"tile\"><div class=\"label\">{}</div><div class=\"num\">{}</div></div>",
                esc_str(label),
                num
            )
        })
        .collect();

    let body = if cards.is_empty() {
        "<div class=\"empty\">No cards yet. Run <code>card.draft.auto</code> or <code>board-keeper.card.add</code>.</div>".to_string()
    } else {
        let rows: String = cards
            .iter()
            .map(|c| {
                let eslug = esc_html(c.get("slug"));
                let etitle = esc_html(c.get("title"));
                // class: escHtml(c.status || 'backlog')
                let eclass = if truthy(c.get("status")) { esc_html(c.get("status")) } else { "backlog".to_string() };
                let estatus = esc_html(c.get("status"));
                // contractCounts[c.slug] || 0 (counts are >= 1 when present)
                let count = contract_counts.get(&prop_key(c.get("slug"))).copied().unwrap_or(0);
                // escHtml((c.created_at ?? '').slice(0, 16))
                let created_raw = match c.get("created_at") {
                    None | Some(Value::Null) => String::new(),
                    Some(Value::String(s)) => s.clone(),
                    Some(o) => js_string(o),
                };
                let created16: String = created_raw.chars().take(16).collect();
                let ecreated = esc_str(&created16);
                format!(
                    "\n          <tr>\n            <td><a href=\"{slug}.html\"><code>{slug}</code></a></td>\n            <td>{title}</td>\n            <td><span class=\"badge status-{cls}\">{status}</span></td>\n            <td>{count}</td>\n            <td class=\"meta\">{created}</td>\n          </tr>",
                    slug = eslug, title = etitle, cls = eclass, status = estatus, count = count, created = ecreated
                )
            })
            .collect();
        format!(
            "<table><thead><tr><th>slug</th><th>title</th><th>status</th><th>contracts</th><th>created</th></tr></thead><tbody>\n        {}\n      </tbody></table>",
            rows
        )
    };

    let mut out = String::from(PREFIX);
    out.push_str(&summary);
    out.push_str("</div>\n");
    out.push_str(&body);
    out.push_str("\n</div></body></html>");
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, created_at TEXT); \
             CREATE TABLE contracts (id INTEGER PRIMARY KEY, card_slug TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn empty_board() {
        let c = seed();
        let r = cardRenderIndexHtml(&json!({}), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.starts_with("<!DOCTYPE html>\n<html><head>"));
        assert!(s.contains("<title>cards \u{b7} board</title>"));
        // preserved source quirk
        assert!(s.contains("BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif"));
        assert!(s.contains("<div class=\"label\">total</div><div class=\"num\">0</div>"));
        assert!(s.contains("<div class=\"label\">with contracts</div><div class=\"num\">0</div>"));
        assert!(s.contains("<div class=\"empty\">No cards yet. Run <code>card.draft.auto</code>"));
        assert!(s.ends_with("</div>\n</div></body></html>"));
    }

    #[test]
    fn populated_board() {
        let c = seed();
        c.execute_batch(
            "INSERT INTO cards VALUES ('FEAT-A','Alpha & <b>bold</b>','doing','2026-05-28T12:34:56Z'); \
             INSERT INTO cards VALUES ('FEAT-D','Delta',NULL,'2026-01-01T00:00:00Z'); \
             INSERT INTO cards VALUES ('FEAT-E','Epsilon','','2026-02-02T02:02:02Z'); \
             INSERT INTO contracts VALUES (1,'FEAT-A'); \
             INSERT INTO contracts VALUES (2,'FEAT-A'); \
             INSERT INTO contracts VALUES (3,NULL);",
        )
        .unwrap();
        let r = cardRenderIndexHtml(&json!({}), &c).unwrap();
        let s = r.as_str().unwrap();
        // HTML escaping of the title
        assert!(s.contains("<td>Alpha &amp; &lt;b&gt;bold&lt;/b&gt;</td>"));
        // slug link + code
        assert!(s.contains("<td><a href=\"FEAT-A.html\"><code>FEAT-A</code></a></td>"));
        // doing badge + contract count 2 + created slice (16 chars)
        assert!(s.contains("<span class=\"badge status-doing\">doing</span>"));
        assert!(s.contains("<td class=\"meta\">2026-05-28T12:34</td>"));
        // null status → class falls back to 'backlog', text empty
        assert!(s.contains("<span class=\"badge status-backlog\"></span>"));
        // empty-string status → same fallback
        assert!(s.contains("<td><a href=\"FEAT-E.html\"><code>FEAT-E</code></a></td>"));
        // summary tallies: total 3, doing 1, backlog 0, with contracts 1 (only FEAT-A)
        assert!(s.contains("<div class=\"label\">total</div><div class=\"num\">3</div>"));
        assert!(s.contains("<div class=\"label\">doing</div><div class=\"num\">1</div>"));
        assert!(s.contains("<div class=\"label\">with contracts</div><div class=\"num\">1</div>"));
        // table structure boundaries
        assert!(s.contains("</thead><tbody>\n        \n          <tr>"));
        assert!(s.ends_with("</tbody></table>\n</div></body></html>"));
    }
}
