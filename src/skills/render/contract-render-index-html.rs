//! contract-render-index-html.rs — twin of skills/render/contract-render-index-html.mjs (db READ lane).
//! NAMES ARE CONTRACT: `contractRenderIndexHtml` (default). Board view of all contracts as static HTML.
// QUALITY: byte-exact template — CSS/SORT_JS consts, escHtml chain, tier/size/risk/card cells, tallies.
use rusqlite::Connection;
use serde_json::{json, Value};

// = JS String(v) — null → "null" (used for direct escHtml(x) and inside truthy branches).
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
// = JS String(v ?? '') — undefined/null → "".
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(x) => js_string(x),
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
// escHtml body: .replace chain in JS order (& first so later entities aren't double-escaped).
fn esc_str(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

// Static fragments generated VERBATIM from the JS output (CSS const, SORT_JS const, head/nav/kicker
// scaffolding). The literal `<body>quot;` in the nav font-family is a byte-for-byte quirk of the JS
// source (a mangled `&quot;`) and is preserved intentionally.
const CSS: &str = "\n:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; }\nbody { font-family: -apple-system, BlinkMacSystemFont, \"Segoe UI\", system-ui, sans-serif; background: var(--bg); color: var(--fg); margin: 0; padding: 0; line-height: 1.55; }\n.wrap { max-width: 1280px; margin: 0 auto; padding: 2em 1.5em 4em; }\nh1 { font-size: 1.7em; font-weight: 700; margin: 0 0 0.2em; }\n.kicker { color: var(--accent); font-family: ui-monospace, monospace; font-size: 0.78em; text-transform: uppercase; letter-spacing: 0.08em; margin-bottom: 0.2em; }\n.meta { color: var(--muted); font-size: 0.85em; margin: 0.3em 0 1em; font-family: ui-monospace, monospace; }\ntable { width: 100%; border-collapse: collapse; margin: 1em 0; font-size: 0.92em; }\nth { background: var(--panel); padding: 0.6em 0.8em; text-align: left; color: var(--muted); border-bottom: 1px solid var(--border); cursor: pointer; user-select: none; }\nth:hover { color: var(--fg); }\ntd { padding: 0.55em 0.8em; border-bottom: 1px solid var(--border); }\ntr:hover td { background: rgba(88,166,255,0.04); }\na { color: var(--accent); text-decoration: none; }\na:hover { text-decoration: underline; }\ncode { font-family: ui-monospace, monospace; font-size: 0.88em; background: rgba(0,0,0,0.3); padding: 1px 5px; border-radius: 3px; }\n.badge { display: inline-block; padding: 2px 8px; border-radius: 10px; font-size: 0.78em; font-family: ui-monospace, monospace; font-weight: 500; white-space: nowrap; }\n.badge.tier-trivial    { background: rgba(63,185,80,0.12);  color: var(--good); border: 1px solid rgba(63,185,80,0.4); }\n.badge.tier-standard   { background: rgba(88,166,255,0.12); color: var(--accent); border: 1px solid rgba(88,166,255,0.4); }\n.badge.tier-high-risk  { background: rgba(248,81,73,0.12);  color: var(--bad); border: 1px solid rgba(248,81,73,0.4); }\n.badge.status-proposed       { background: rgba(139,148,158,0.12); color: var(--muted); border: 1px solid var(--border); }\n.badge.status-tl-approved    { background: rgba(210,153,34,0.12);  color: var(--warn); border: 1px solid rgba(210,153,34,0.4); }\n.badge.status-human-approved { background: rgba(63,185,80,0.12);   color: var(--good); border: 1px solid rgba(63,185,80,0.4); }\n.badge.status-impl    { background: rgba(88,166,255,0.12); color: var(--accent); border: 1px solid rgba(88,166,255,0.4); }\n.badge.status-qa      { background: rgba(188,140,255,0.12); color: #bc8cff; border: 1px solid rgba(188,140,255,0.4); }\n.badge.status-done    { background: rgba(63,185,80,0.18); color: var(--good); border: 1px solid rgba(63,185,80,0.5); }\n.badge.status-rejected,.badge.status-superseded { background: rgba(248,81,73,0.12); color: var(--bad); border: 1px solid rgba(248,81,73,0.4); }\n.empty { background: var(--panel); border: 1px solid var(--border); border-radius: 8px; padding: 2em; text-align: center; color: var(--muted); margin: 2em 0; }\n.summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 0.8em; margin: 1em 0; }\n.tile { background: var(--panel); border: 1px solid var(--border); padding: 0.8em 1em; border-radius: 6px; }\n.tile .label { color: var(--muted); font-size: 0.78em; }\n.tile .num { font-size: 1.5em; font-weight: 600; color: var(--accent); }\n";
const SORT_JS: &str = "\nfunction sortTable(idx) {\n  const table = document.getElementById('contracts');\n  const tbody = table.tBodies[0];\n  const rows = Array.from(tbody.rows);\n  const dir = table.dataset.sortIdx === String(idx) && table.dataset.sortDir === 'asc' ? 'desc' : 'asc';\n  rows.sort((a, b) => {\n    const av = a.cells[idx].dataset.sort ?? a.cells[idx].textContent;\n    const bv = b.cells[idx].dataset.sort ?? b.cells[idx].textContent;\n    const cmp = isNaN(av - bv) ? av.localeCompare(bv) : (av - bv);\n    return dir === 'asc' ? cmp : -cmp;\n  });\n  rows.forEach((r) => tbody.appendChild(r));\n  table.dataset.sortIdx = String(idx);\n  table.dataset.sortDir = dir;\n}\n";
const P1: &str = "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"UTF-8\">\n<title>contracts \u{2014} board view</title>\n<style>";
const P2: &str = "</style>\n<script>";
const P3: &str = "</script>\n</head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"../index.html\" style=\"color:#58a6ff;text-decoration:none;\">\u{2190} antiloki docs</a></nav>\n<div class=\"wrap\">\n  <div class=\"kicker\">antiloki-v3 \u{b7} contracts board</div>\n  <h1>All contracts</h1>\n  <div class=\"meta\">Auto-generated \u{b7} regen: <code>node bin/antiloki.mjs contract.render.index.html '{}' &gt; docs/contracts/index.html</code></div>\n  <div class=\"summary\">";
const P4: &str = "</div>\n  ";
const P5: &str = "\n</div>\n</body>\n</html>";
const EMPTY_BODY: &str = "<div class=\"empty\">No contracts yet. File a card, then run <code>contract.draft.auto</code> \u{2192} <code>contract.gate.tl.sign</code> with decision=accept.</div>";
const TABLE_PRE: &str = "\n      <table id=\"contracts\" data-sort-idx=\"0\" data-sort-dir=\"asc\">\n        <thead>\n          <tr>\n            <th onclick=\"sortTable(0)\">slug</th>\n            <th onclick=\"sortTable(1)\">status</th>\n            <th onclick=\"sortTable(2)\">tier</th>\n            <th onclick=\"sortTable(3)\">size</th>\n            <th onclick=\"sortTable(4)\">risk</th>\n            <th onclick=\"sortTable(5)\">card</th>\n            <th onclick=\"sortTable(6)\">attempt</th>\n            <th onclick=\"sortTable(7)\">created</th>\n          </tr>\n        </thead>\n        <tbody>\n          ";
const TABLE_POST: &str = "\n        </tbody>\n      </table>\n    ";
const DASH: &str = "<span class=\"meta\">\u{2014}</span>";

fn row_html(c: &Value) -> String {
    let slug = esc_str(&js_string(c.get("slug").unwrap_or(&Value::Null))); // escHtml(c.slug)
    let status_esc = esc_str(&js_string(c.get("status").unwrap_or(&Value::Null))); // escHtml(c.status)
    // status-${escHtml((c.status||'').replace(/_/g,'-'))}
    let status_class = {
        let s = if truthy(c.get("status")) { js_string(c.get("status").unwrap()) } else { String::new() };
        esc_str(&s.replace('_', "-"))
    };
    let tier_sort = esc_str(&str_qq(c.get("tier"))); // escHtml(c.tier ?? '')
    let tier_cell = if truthy(c.get("tier")) {
        let t = js_string(c.get("tier").unwrap());
        format!("<span class=\"badge tier-{}\">{}</span>", esc_str(&t.replace('_', "-")), esc_str(&t))
    } else { DASH.to_string() };
    let size_sort = esc_str(&str_qq(c.get("size"))); // escHtml(c.size ?? '')
    let size_cell = if truthy(c.get("size")) {
        format!("<code>{}</code>", esc_str(&js_string(c.get("size").unwrap())))
    } else { DASH.to_string() };
    // data-sort="${c.risk ?? -1}"; cell: c.risk != null ? `${c.risk}/5` : dash
    let risk_sort = match nz(c.get("risk")) { None => "-1".to_string(), Some(v) => js_string(v) };
    let risk_cell = match nz(c.get("risk")) { None => DASH.to_string(), Some(v) => format!("{}/5", js_string(v)) };
    let card_cell = if truthy(c.get("card_slug")) {
        format!("<code>{}</code>", esc_str(&js_string(c.get("card_slug").unwrap())))
    } else { DASH.to_string() };
    let attempt = match nz(c.get("current_attempt")) { None => "1".to_string(), Some(v) => js_string(v) }; // ${c.current_attempt ?? 1}
    let created = str_qq(c.get("created_at")); // c.created_at ?? ''
    let created_sort = esc_str(&created);
    let created16: String = created.chars().take(16).collect(); // .slice(0, 16)
    let created_text = esc_str(&created16);
    format!(
        "\n            <tr>\n              <td><a href=\"{slug}.html\"><code>{slug}</code></a></td>\n              <td data-sort=\"{ss}\"><span class=\"badge status-{sc}\">{ss}</span></td>\n              <td data-sort=\"{ts}\">{tc}</td>\n              <td data-sort=\"{szs}\">{szc}</td>\n              <td data-sort=\"{rs}\">{rc}</td>\n              <td>{cc}</td>\n              <td>{at}</td>\n              <td class=\"meta\" data-sort=\"{crs}\">{crt}</td>\n            </tr>",
        slug = slug, ss = status_esc, sc = status_class, ts = tier_sort, tc = tier_cell,
        szs = size_sort, szc = size_cell, rs = risk_sort, rc = risk_cell, cc = card_cell,
        at = attempt, crs = created_sort, crt = created_text,
    )
}

pub fn contractRenderIndexHtml(_input: &Value, db: &Connection) -> Result<Value, String> {
    let listed = crate::contract_contract_list::contractList(&json!({ "limit": 1000 }), db)?;
    let contracts = listed.get("contracts").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // byStatus / byTier tallies (only the read keys matter).
    let (mut n_tl, mut n_impl, mut n_done, mut n_hr) = (0usize, 0usize, 0usize, 0usize);
    for c in &contracts {
        match c.get("status").and_then(|v| v.as_str()) {
            Some("tl_approved") => n_tl += 1,
            Some("impl") => n_impl += 1,
            Some("done") => n_done += 1,
            _ => {}
        }
        if c.get("tier").and_then(|v| v.as_str()) == Some("high-risk") { n_hr += 1; }
    }

    let tiles_data: [(&str, usize); 5] = [
        ("total active", contracts.len()),
        ("tl_approved", n_tl),
        ("impl", n_impl),
        ("done", n_done),
        ("high-risk", n_hr),
    ];
    let tiles: String = tiles_data
        .iter()
        .map(|&(label, num)| format!("<div class=\"tile\"><div class=\"label\">{}</div><div class=\"num\">{}</div></div>", esc_str(label), num))
        .collect();

    let body = if contracts.is_empty() {
        EMPTY_BODY.to_string()
    } else {
        let rows: String = contracts.iter().map(row_html).collect();
        format!("{}{}{}", TABLE_PRE, rows, TABLE_POST)
    };

    let mut out = String::from(P1);
    out.push_str(CSS);
    out.push_str(P2);
    out.push_str(SORT_JS);
    out.push_str(P3);
    out.push_str(&tiles);
    out.push_str(P4);
    out.push_str(&body);
    out.push_str(P5);
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(rows: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT, title TEXT, status TEXT, template_kind TEXT, size TEXT, complexity TEXT, risk INTEGER, tier TEXT, card_slug TEXT, current_attempt INTEGER, superseded_by TEXT, signed_by TEXT, signed_at TEXT, created_at TEXT, updated_at TEXT);",
        )
        .unwrap();
        if !rows.is_empty() { c.execute_batch(rows).unwrap(); }
        c
    }

    #[test]
    fn empty_board() {
        let c = seed("");
        let r = contractRenderIndexHtml(&json!({}), &c).unwrap();
        let s = r.as_str().unwrap();
        assert!(s.starts_with("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n"));
        assert!(s.contains("<title>contracts \u{2014} board view</title>"));
        // preserved source quirk
        assert!(s.contains("BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif"));
        assert!(s.contains("<div class=\"label\">total active</div><div class=\"num\">0</div>"));
        assert!(s.contains("No contracts yet. File a card"));
        assert!(!s.contains("<table"));
        assert!(s.ends_with("\n</div>\n</body>\n</html>"));
    }

    #[test]
    fn populated_board() {
        let c = seed(
            "INSERT INTO contracts (slug,status,size,risk,tier,card_slug,current_attempt,created_at) VALUES \
               ('CONTRACT-A','impl','S',3,'standard','CARD-1',2,'2026-07-21T10:00:00Z'), \
               ('CONTRACT-B','tl_approved',NULL,NULL,NULL,NULL,NULL,NULL), \
               ('CONTRACT-C','done','XL',0,'high-risk','CARD-3',1,'2026-07-22T09:30:45Z'), \
               ('A&B<x>\"q\"','human_approved','M',5,'trivial','CARD&4',1,'2026-07-23T00:00:00Z');",
        );
        let r = contractRenderIndexHtml(&json!({}), &c).unwrap();
        let s = r.as_str().unwrap();
        // slug link + status badge (underscore→dash in class)
        assert!(s.contains("<td><a href=\"CONTRACT-A.html\"><code>CONTRACT-A</code></a></td>"));
        assert!(s.contains("<td data-sort=\"impl\"><span class=\"badge status-impl\">impl</span></td>"));
        assert!(s.contains("<td data-sort=\"tl_approved\"><span class=\"badge status-tl-approved\">tl_approved</span></td>"));
        assert!(s.contains("<td data-sort=\"human_approved\"><span class=\"badge status-human-approved\">human_approved</span></td>"));
        // tier badge + high-risk
        assert!(s.contains("<td data-sort=\"standard\"><span class=\"badge tier-standard\">standard</span></td>"));
        assert!(s.contains("<td data-sort=\"high-risk\"><span class=\"badge tier-high-risk\">high-risk</span></td>"));
        // risk: 3/5, 0/5, and null → data-sort=-1 + dash
        assert!(s.contains("<td data-sort=\"3\">3/5</td>"));
        assert!(s.contains("<td data-sort=\"0\">0/5</td>"));
        assert!(s.contains("<td data-sort=\"-1\"><span class=\"meta\">\u{2014}</span></td>"));
        // null tier/size render the em-dash meta cell
        assert!(s.contains("<td data-sort=\"\"><span class=\"meta\">\u{2014}</span></td>"));
        // current_attempt ?? 1 (row B is null → 1)
        assert!(s.contains("<td>1</td>"));
        // card_slug escaped, created slice(0,16)
        assert!(s.contains("<td><code>CARD&amp;4</code></td>"));
        assert!(s.contains("<td class=\"meta\" data-sort=\"2026-07-21T10:00:00Z\">2026-07-21T10:00</td>"));
        // slug HTML-escaping
        assert!(s.contains("<td><a href=\"A&amp;B&lt;x&gt;&quot;q&quot;.html\"><code>A&amp;B&lt;x&gt;&quot;q&quot;</code></a></td>"));
        // summary tallies: total 4, tl_approved 1, impl 1, done 1, high-risk 1
        assert!(s.contains("<div class=\"label\">total active</div><div class=\"num\">4</div>"));
        assert!(s.contains("<div class=\"label\">high-risk</div><div class=\"num\">1</div>"));
        assert!(s.contains("<table id=\"contracts\" data-sort-idx=\"0\" data-sort-dir=\"asc\">"));
        assert!(s.ends_with("\n        </tbody>\n      </table>\n    \n</div>\n</body>\n</html>"));
    }
}
