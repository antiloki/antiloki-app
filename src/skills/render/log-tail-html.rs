//! log-tail-html.rs — twin of skills/render/log-tail-html.mjs (db READ lane). NAMES ARE CONTRACT:
//! `logTailHtml` (default). Auto-refreshing HTML page of the last N runtime_events (meta-refresh),
//! rendered as a styled table with a quick-stats summary. Sibling of log-tail-ansi (same filters).
// QUALITY: 80/100 — byte-exact HTML template; escHtml order (& < > "); toFixed(2) dur; DESC, no reverse
use rusqlite::Connection;
use serde_json::{json, Value};

// The two verbatim blobs, transcribed byte-for-byte from the JS cooked template value (CRLF→LF
// normalized, exactly as a JS template literal cooks its source). Do not hand-edit.
const CSS: &str = r####"
:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e;
        --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; --code-bg:#0d1117; --rag:#bc8cff; }
body { font-family: ui-monospace, "JetBrains Mono", Consolas, monospace; background: var(--bg); color: var(--fg); margin: 0; padding: 1em 1.5em; font-size: 13px; }
h1 { font-size: 1.2em; margin: 0 0 0.4em; }
.meta { color: var(--muted); font-size: 0.85em; margin: 0 0 1em; }
table { width: 100%; border-collapse: collapse; }
th { background: var(--panel); padding: 0.4em 0.6em; text-align: left; color: var(--muted); font-weight: 500; font-size: 11px; border-bottom: 1px solid var(--border); }
td { padding: 0.35em 0.6em; border-bottom: 1px solid var(--border); white-space: nowrap; }
tr:hover td { background: rgba(88,166,255,0.04); }
.id { color: var(--muted); }
.ts { color: var(--muted); font-size: 0.85em; }
.dur-fast   { color: var(--muted); }
.dur-mid    { color: var(--warn); }
.dur-slow   { color: var(--bad); }
.action      { color: var(--accent); }
.skill   { color: var(--rag); }
.outcome    { padding: 1px 6px; border-radius: 9px; font-size: 11px; display: inline-block; }
.outcome-success   { background: rgba(63,185,80,0.12); color: var(--good); border: 1px solid rgba(63,185,80,0.4); }
.outcome-error,
.outcome-blocked   { background: rgba(248,81,73,0.12); color: var(--bad); border: 1px solid rgba(248,81,73,0.4); }
.outcome-in_flight { background: rgba(210,153,34,0.12); color: var(--warn); border: 1px solid rgba(210,153,34,0.4); }
.outcome-skipped   { background: var(--panel); color: var(--muted); border: 1px solid var(--border); }
.parent { color: var(--muted); font-size: 11px; }
.empty { color: var(--muted); text-align: center; padding: 3em; }
.summary { display: flex; gap: 0.7em; margin: 0.5em 0 1em; flex-wrap: wrap; }
.chip { background: var(--panel); border: 1px solid var(--border); padding: 0.3em 0.7em; border-radius: 4px; font-size: 11px; }
.chip strong { color: var(--accent); }
"####;
const NAV: &str = r####"</head><body><nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,</head><body>quot;Segoe UI</head><body>quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav><nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>"####;

// String(v ?? '') — the value that reaches escHtml / interpolation from a db cell (string|number|null).
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `${v}` interpolation (null → "null"; number → its text; string → itself).
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
// escHtml: String(s ?? '') then &, <, >, " — in THAT order (& first, so we never double-escape).
fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn esc_html(v: Option<&Value>) -> String { escape(&str_qq(v)) }
// JS String#slice(start,end) over UTF-16 code units; BMP-equivalent via chars for ASCII fixtures.
fn slice_chars(s: &str, start: usize, end: usize) -> String { s.chars().skip(start).take(end.saturating_sub(start)).collect() }

fn dur_class(ms: Option<&Value>) -> &'static str {
    match ms {
        None | Some(Value::Null) => "dur-fast",
        Some(v) => {
            let n = v.as_f64().unwrap_or(f64::NAN);
            if n > 1000.0 { "dur-slow" } else if n > 100.0 { "dur-mid" } else { "dur-fast" }
        }
    }
}

fn build_row(r: &Value) -> String {
    let ts_str = str_qq(r.get("ts"));
    let esc_ts = escape(&slice_chars(&ts_str, 11, 23));
    let dur_cell = match r.get("duration_ms") {
        None | Some(Value::Null) => "\u{2014}".to_string(),
        Some(v) => format!("{:.2}ms", v.as_f64().unwrap_or(f64::NAN)),
    };
    let esc_out = esc_html(r.get("outcome"));
    let esc_agent = if r.get("agent").map_or(true, |v| v.is_null()) { "-".to_string() } else { esc_html(r.get("agent")) };
    let what = if truthy(r.get("action")) {
        format!("<span class=\"action\">{}</span>", esc_html(r.get("action")))
    } else if truthy(r.get("skill")) {
        format!("<span class=\"skill\">{}</span>", esc_html(r.get("skill")))
    } else {
        "<span class=\"ts\">?</span>".to_string()
    };
    let parent = match r.get("parent_event_id") {
        None | Some(Value::Null) => String::new(),
        Some(v) => tmpl(Some(v)),
    };
    let err_cell = if truthy(r.get("error_code")) {
        format!("<span class=\"outcome outcome-error\">{}</span>", esc_html(r.get("error_code")))
    } else {
        String::new()
    };
    format!(
        "\n          <tr>\n            <td class=\"id\">{id}</td>\n            <td class=\"ts\">{ts}</td>\n            <td class=\"{durcls}\">{durcell}</td>\n            <td><span class=\"outcome outcome-{out}\">{out}</span></td>\n            <td>{agent}</td>\n            <td>{what}</td>\n            <td class=\"parent\">{parent}</td>\n            <td>{err}</td>\n          </tr>\n        ",
        id = tmpl(r.get("id")), ts = esc_ts, durcls = dur_class(r.get("duration_ms")),
        durcell = dur_cell, out = esc_out, agent = esc_agent, what = what, parent = parent, err = err_cell
    )
}

pub fn logTailHtml(input: &Value, db: &Connection) -> Result<Value, String> {
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(100).clamp(1, 500);
    let sql = "SELECT id, ts, parent_event_id, actor, agent, action, skill, duration_ms, outcome, error_code FROM runtime_events ORDER BY id DESC LIMIT ?";
    let rows = crate::db_json::query_rows(db, sql, &[json!(limit)])?.as_array().cloned().unwrap_or_default();

    let refresh = match input.get("refresh_seconds") { None => "5".to_string(), Some(v) => tmpl(Some(v)) };

    let (mut success, mut error, mut blocked, mut in_flight) = (0i64, 0i64, 0i64, 0i64);
    for r in &rows {
        match r.get("outcome").and_then(|v| v.as_str()) {
            Some("success") => success += 1,
            Some("error") => error += 1,
            Some("blocked") => blocked += 1,
            Some("in_flight") => in_flight += 1,
            _ => {}
        }
    }
    let mut slow = 0i64;
    for r in &rows {
        let d = match r.get("duration_ms") { None | Some(Value::Null) => 0.0, Some(v) => v.as_f64().unwrap_or(f64::NAN) };
        if d > 100.0 { slow += 1; }
    }
    let n = rows.len();

    let summary = format!(
        "\n    <div class=\"summary\">\n      <span class=\"chip\">total <strong>{}</strong></span>\n      <span class=\"chip\">success <strong>{}</strong></span>\n      <span class=\"chip\">error <strong>{}</strong></span>\n      <span class=\"chip\">blocked <strong>{}</strong></span>\n      <span class=\"chip\">in_flight <strong>{}</strong></span>\n      <span class=\"chip\">slow (&gt;100ms) <strong>{}</strong></span>\n    </div>\n  ",
        n, success, error, blocked, in_flight, slow
    );

    let body = if n == 0 {
        "<div class=\"empty\">No runtime_events yet. Dispatch any action to populate.</div>".to_string()
    } else {
        let joined: String = rows.iter().map(build_row).collect();
        format!(
            "<table>\n        <thead><tr>\n          <th>id</th><th>ts</th><th>dur</th><th>outcome</th><th>agent</th><th>what</th><th>parent</th><th>err</th>\n        </tr></thead>\n        <tbody>\n        {}\n        </tbody>\n      </table>",
            joined
        )
    };

    let out = format!(
        "<!DOCTYPE html>\n<html><head>\n<meta charset=\"UTF-8\">\n<meta http-equiv=\"refresh\" content=\"{refresh}\">\n<title>antiloki \u{00b7} runtime tail</title>\n<style>{css}</style>\n{nav}\n<h1>antiloki runtime events</h1>\n<div class=\"meta\">last {n} \u{00b7} auto-refresh {refresh}s \u{00b7} regen: <code>node bin/antiloki.mjs system.log.tail.html &gt; docs/runtime.html</code></div>\n{summary}\n{body}\n</body></html>",
        refresh = refresh, css = CSS, nav = NAV, n = n, summary = summary, body = body
    );
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, parent_event_id INTEGER, actor TEXT, agent TEXT, action TEXT, skill TEXT, duration_ms INTEGER, outcome TEXT, error_code TEXT); \
          INSERT INTO runtime_events (id,ts,parent_event_id,actor,agent,action,skill,duration_ms,outcome,error_code) VALUES \
          (1,'2026-01-01 12:00:00.000',NULL,'a','pl','card.add','card-add',50,'success',NULL), \
          (2,'2026-01-01 12:00:01.500',1,'b','tl','contract.sign','contract-sign',1200,'error','E_X'), \
          (3,NULL,NULL,'c',NULL,NULL,'sub-skill',150,'in_flight',NULL), \
          (4,'2026-01-01 12:00:03.000',2,'d','qa',NULL,NULL,NULL,'blocked','E_Y'), \
          (5,'2026-01-01 12:00:04.250',NULL,'e','dev','file.write<>&\"','file-write',99,'skipped',NULL);").unwrap();
        c
    }

    #[test]
    fn renders_full_page() {
        let c = seeded();
        let out = logTailHtml(&json!({}), &c).unwrap();
        let h = out.as_str().unwrap();
        // shell + defaults
        assert!(h.starts_with("<!DOCTYPE html>\n<html><head>\n"));
        assert!(h.ends_with("\n</body></html>"));
        assert!(h.contains("<meta http-equiv=\"refresh\" content=\"5\">"));
        assert!(h.contains("<title>antiloki \u{00b7} runtime tail</title>"));
        assert!(h.contains("last 5 \u{00b7} auto-refresh 5s \u{00b7} regen"));
        assert!(h.contains(CSS) && h.contains(NAV));
        // quick-stats summary (one of each outcome; 2 rows over 100ms: 1200 & 150)
        assert!(h.contains("total <strong>5</strong>"));
        assert!(h.contains("success <strong>1</strong>"));
        assert!(h.contains("error <strong>1</strong>"));
        assert!(h.contains("blocked <strong>1</strong>"));
        assert!(h.contains("in_flight <strong>1</strong>"));
        assert!(h.contains("slow (&gt;100ms) <strong>2</strong>"));
        // duration classes + toFixed(2) + null em-dash
        assert!(h.contains("<td class=\"dur-slow\">1200.00ms</td>"));
        assert!(h.contains("<td class=\"dur-mid\">150.00ms</td>"));
        assert!(h.contains("<td class=\"dur-fast\">99.00ms</td>"));
        assert!(h.contains("<td class=\"dur-fast\">\u{2014}</td>"));
        // outcome span (class + text), error-code cell
        assert!(h.contains("<span class=\"outcome outcome-blocked\">blocked</span>"));
        assert!(h.contains("<span class=\"outcome outcome-error\">E_X</span>"));
        // what: action span > skill span > '?'
        assert!(h.contains("<span class=\"action\">card.add</span>"));
        assert!(h.contains("<span class=\"skill\">sub-skill</span>")); // id=3: action null, skill set
        assert!(h.contains("<span class=\"ts\">?</span>"));            // id=4: both null
        // agent ?? '-' and escHtml order (& < > ")
        assert!(h.contains("<td>-</td>"));
        assert!(h.contains("<span class=\"action\">file.write&lt;&gt;&amp;&quot;</span>"));
        // parent cell present + DESC order (id 5 before id 1)
        assert!(h.contains("<td class=\"parent\">1</td>"));
        assert!(h.find("<td class=\"id\">5</td>").unwrap() < h.find("<td class=\"id\">1</td>").unwrap());
    }

    #[test]
    fn empty_and_limit() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, parent_event_id INTEGER, actor TEXT, agent TEXT, action TEXT, skill TEXT, duration_ms INTEGER, outcome TEXT, error_code TEXT);").unwrap();
        let empty = logTailHtml(&json!({}), &c).unwrap();
        let es = empty.as_str().unwrap();
        assert!(es.contains("<div class=\"empty\">No runtime_events yet. Dispatch any action to populate.</div>"));
        assert!(es.contains("total <strong>0</strong>"));
        assert!(es.contains("content=\"5\""));

        let c2 = seeded();
        let two = logTailHtml(&json!({ "limit": 2, "refresh_seconds": 30 }), &c2).unwrap();
        let s = two.as_str().unwrap();
        assert!(s.contains("content=\"30\""));
        assert!(s.contains("auto-refresh 30s"));
        assert!(s.contains("total <strong>2</strong>"));
        assert_eq!(s.matches("<td class=\"id\">").count(), 2); // limit clamps to 2 body rows
    }
}
