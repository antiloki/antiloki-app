//! man-compliance-html.rs — twin of skills/render/man-compliance-html.mjs (db READ lane). NAMES ARE
//! CONTRACT: `manComplianceHtml` (default). MAN dashboard: renders man-audit-drift + recent qa_runs
//! as a stoplight HTML page. Delegates the drift roll-up to the already-converted drift twin.
// QUALITY: template literals normalize CRLF->LF at parse time, so the runtime HTML uses '\n' even
// though the source is CRLF; static segments captured cooked; escHtml + toFixed(0) reproduced byte-exact.
use rusqlite::Connection;
use serde_json::{json, Value};

// ---- JS coercion helpers ----
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}

// `${number}` for a JSON number value (int renders "5", fractional "0.5").
fn num_str(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
// JS `${x}` for a computed number (integer-valued -> no decimal point).
fn js_num_str(x: f64) -> String {
    if x.is_nan() { return "NaN".into(); }
    if x.is_infinite() { return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() }; }
    if x.fract() == 0.0 && x.abs() < 9e15 { format!("{}", x as i64) } else { format!("{}", x) }
}
// JS Number.prototype.toFixed(0): round to nearest integer, ties toward +inf. Domain here: x >= 0.
fn to_fixed0(x: f64) -> String {
    if x.is_nan() { return "NaN".into(); }
    if x.is_infinite() { return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() }; }
    format!("{}", (x + 0.5).floor() as i64)
}

// escHtml(s) = String(s ?? '').replace(&,<,>,") in that order.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn esc_val(v: Option<&Value>) -> String { esc(&str_qq(v)) }

// ---- static template segments (captured cooked; CRLF already normalized to LF) ----
const CSS: &str = "\n:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; }\nbody { font-family:-apple-system,BlinkMacSystemFont,system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }\n.wrap { max-width:1080px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.7em; margin:0 0 0.2em; }\nh2 { font-size:1.25em; margin-top:2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; }\n.meta { color:var(--muted); font-size:0.85em; margin:0.3em 0 1em; font-family:ui-monospace,monospace; }\n.hero { background:var(--panel); border:1px solid; border-radius:10px; padding:2em; text-align:center; margin:1em 0; }\n.hero.green  { border-color:var(--good); }\n.hero.yellow { border-color:var(--warn); }\n.hero.red    { border-color:var(--bad); }\n.hero .lamp { font-size:5em; line-height:1; }\n.hero .label { color:var(--muted); font-size:0.8em; font-family:ui-monospace,monospace; }\n.hero .sub   { color:var(--muted); font-size:0.95em; }\ntable { width:100%; border-collapse:collapse; margin:1em 0; font-size:0.92em; }\nth,td { padding:0.55em 0.8em; text-align:left; border-bottom:1px solid var(--border); }\nth { color:var(--muted); background:var(--panel); font-weight:500; font-size:0.85em; }\ncode { font-family:ui-monospace,monospace; font-size:0.88em; background:rgba(0,0,0,0.3); padding:1px 5px; border-radius:3px; }\n.cov-bar { display:inline-block; height:6px; background:var(--panel); border-radius:3px; width:80px; overflow:hidden; vertical-align:middle; margin-left:0.4em; }\n.cov-bar > div { height:100%; background:var(--good); }\n.tag { padding:1px 7px; border-radius:10px; font-size:0.78em; font-family:ui-monospace,monospace; }\n.tag.ok   { background:rgba(63,185,80,0.15); color:var(--good); border:1px solid rgba(63,185,80,0.4); }\n.tag.warn { background:rgba(210,153,34,0.15); color:var(--warn); border:1px solid rgba(210,153,34,0.4); }\n.tag.bad  { background:rgba(248,81,73,0.15); color:var(--bad); border:1px solid rgba(248,81,73,0.4); }\n.muted { color:var(--muted); }\n";

const S0: &str = "<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>antiloki · MAN compliance</title><style>";
const S1: &str = "</style></head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"./index.html\" style=\"color:#58a6ff;text-decoration:none;\">← antiloki docs</a></nav><div class=\"wrap\">\n<div class=\"kicker\">antiloki-v3 · MAN compliance dashboard</div>\n<h1>Substrate health — drift + QA roll-up</h1>\n<p class=\"meta\">Window: <code>";
const S2: &str = "</code> · regen: <code>node bin/antiloki.mjs man.render.compliance.html '{}' &gt; docs/man-compliance.html</code></p>\n\n<div class=\"hero ";
const S3: &str = "\">\n  <div class=\"lamp\">";
const S4: &str = "</div>\n  <div class=\"label\">overall_health</div>\n  <div class=\"sub\">";
const S5: &str = " agents · mean coverage ";
const S6: &str = "% · total drift: ";
const S7: &str = " unauthorized + ";
const S8: &str = " skipped</div>\n  ";
const S9: &str = "\n</div>\n\n<h2>Per-agent compliance</h2>\n<table>\n  <tr><th>agent</th><th>coverage</th><th>unauthorized</th><th>skipped</th><th>summary</th></tr>\n  ";
const S10: &str = "\n</table>\n\n<h2>Recent QA runs (last 10)</h2>\n<table>\n  <tr><th>contract</th><th>ts</th><th>verdict</th><th>AC pass/total</th><th>verifier</th></tr>\n  ";
const S11: &str = "\n</table>\n</div></body></html>";

// worst-offender block (nested template, rendered only when agent_name is truthy)
const W0: &str = "<p class=\"muted\">worst offender: <code>";
const W1: &str = "</code> (drift_count=";
const W2: &str = ")</p>";

// per-agent <tr> row segments
const P0: &str = "<tr>\n      <td><code>";
const P1: &str = "</code></td>\n      <td>";
const P2: &str = "%<span class=\"cov-bar\"><div style=\"width:";
const P3: &str = "%\"></div></span></td>\n      <td><span class=\"tag ";
const P4: &str = "\">";
const P5: &str = "</span></td>\n      <td><span class=\"tag ";
const P6: &str = "\">";
const P7: &str = "</span></td>\n      <td class=\"muted\">";
const P8: &str = "</td>\n    </tr>";

// qa-runs row segments (empty-state row + per-row template)
const QA_EMPTY: &str = "<tr><td colspan=\"5\" class=\"muted\">No QA runs recorded yet.</td></tr>";
const Q0: &str = "\n      <tr>\n        <td><code>";
const Q1: &str = "</code></td>\n        <td class=\"muted\">";
const Q2: &str = "</td>\n        <td><span class=\"tag ";
const Q3: &str = "\">";
const Q4: &str = "</span></td>\n        <td>";
const Q5: &str = "/";
const Q6: &str = "</td>\n        <td class=\"muted\"><code>";
const Q7: &str = "</code></td>\n      </tr>";

pub fn manComplianceHtml(input: &Value, db: &Connection) -> Result<Value, String> {
    // manAuditDrift(input ?? {}, { db })
    let drift_input = if input.is_null() { json!({}) } else { input.clone() };
    let drift = crate::agents_man_man_audit_drift::manAuditDrift(&drift_input, db)?;

    // try { ...qa_runs } catch { [] }
    let recent_qa = crate::db_json::query_rows(
        db,
        "SELECT contract_slug, ts, all_passed, acceptance_pass, acceptance_fail, verifier FROM qa_runs ORDER BY id DESC LIMIT 10",
        &[],
    ).unwrap_or_else(|_| json!([]));
    let recent_qa = recent_qa.as_array().cloned().unwrap_or_default();

    // perAgentRows = drift.per_agent.map(...).join('')
    let mut per_agent_rows = String::new();
    if let Some(arr) = drift["per_agent"].as_array() {
        for a in arr {
            let sum = js_number(Some(&a["unauthorized"])) + js_number(Some(&a["skipped"]));
            let tag = if sum == 0.0 { "ok" } else if sum <= 2.0 { "warn" } else { "bad" };
            let cov = to_fixed0(js_number(Some(&a["coverage_pct"])) * 100.0);
            per_agent_rows.push_str(P0);
            per_agent_rows.push_str(&esc_val(Some(&a["name"])));
            per_agent_rows.push_str(P1);
            per_agent_rows.push_str(&cov);
            per_agent_rows.push_str(P2);
            per_agent_rows.push_str(&cov);
            per_agent_rows.push_str(P3);
            per_agent_rows.push_str(tag);
            per_agent_rows.push_str(P4);
            per_agent_rows.push_str(&num_str(&a["unauthorized"]));
            per_agent_rows.push_str(P5);
            per_agent_rows.push_str(tag);
            per_agent_rows.push_str(P6);
            per_agent_rows.push_str(&num_str(&a["skipped"]));
            per_agent_rows.push_str(P7);
            per_agent_rows.push_str(&esc_val(Some(&a["summary"])));
            per_agent_rows.push_str(P8);
        }
    }

    // qaRows
    let qa_rows = if recent_qa.is_empty() {
        QA_EMPTY.to_string()
    } else {
        let mut acc = String::new();
        for r in &recent_qa {
            let all_passed = truthy(Some(&r["all_passed"]));
            // (r.ts ?? '').slice(0, 16)
            let ts_base = match nz(Some(&r["ts"])) { Some(v) => str_qq(Some(v)), None => String::new() };
            let ts_slice: String = ts_base.chars().take(16).collect();
            let sum = js_number(Some(&r["acceptance_pass"])) + js_number(Some(&r["acceptance_fail"]));
            acc.push_str(Q0);
            acc.push_str(&esc_val(Some(&r["contract_slug"])));
            acc.push_str(Q1);
            acc.push_str(&esc(&ts_slice));
            acc.push_str(Q2);
            acc.push_str(if all_passed { "ok" } else { "bad" });
            acc.push_str(Q3);
            acc.push_str(if all_passed { "pass" } else { "fail" });
            acc.push_str(Q4);
            acc.push_str(&num_str(&r["acceptance_pass"]));
            acc.push_str(Q5);
            acc.push_str(&js_num_str(sum));
            acc.push_str(Q6);
            acc.push_str(&esc_val(Some(&r["verifier"])));
            acc.push_str(Q7);
        }
        acc
    };

    let lamp = match drift["overall_health"].as_str().unwrap_or("") {
        "green" => "🟢",
        "yellow" => "🟡",
        "red" => "🔴",
        _ => "⚪",
    };
    let worst_block = if truthy(Some(&drift["worst_offender"]["agent_name"])) {
        format!(
            "{}{}{}{}{}",
            W0,
            esc_val(Some(&drift["worst_offender"]["agent_name"])),
            W1,
            num_str(&drift["worst_offender"]["drift_count"]),
            W2
        )
    } else {
        String::new()
    };
    let mean_cov = to_fixed0(js_number(Some(&drift["mean_coverage_pct"])) * 100.0);

    let mut out = String::new();
    out.push_str(S0);
    out.push_str(CSS);
    out.push_str(S1);
    out.push_str(&esc_val(Some(&drift["window"]["since_ts"])));
    out.push_str(S2);
    out.push_str(drift["overall_health"].as_str().unwrap_or(""));
    out.push_str(S3);
    out.push_str(lamp);
    out.push_str(S4);
    out.push_str(&num_str(&drift["agents_audited"]));
    out.push_str(S5);
    out.push_str(&mean_cov);
    out.push_str(S6);
    out.push_str(&num_str(&drift["total_unauthorized_skills"]));
    out.push_str(S7);
    out.push_str(&num_str(&drift["total_skipped_always_skills"]));
    out.push_str(S8);
    out.push_str(&worst_block);
    out.push_str(S9);
    out.push_str(&per_agent_rows);
    out.push_str(S10);
    out.push_str(&qa_rows);
    out.push_str(S11);

    Ok(Value::String(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE agent_stages (id INTEGER, agent_name TEXT, stage_name TEXT, order_index INTEGER); \
             CREATE TABLE agent_stage_actions (agent_stage_id INTEGER, action_name TEXT, invocation_pattern TEXT, order_index INTEGER); \
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, ts TEXT, agent TEXT, action TEXT, outcome TEXT, duration_ms INTEGER); \
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, ts TEXT, all_passed INTEGER, acceptance_pass INTEGER, acceptance_fail INTEGER, verifier TEXT);",
        ).unwrap();
        c
    }

    // ground-truth captured by running the JS skill (harness) on the identical seed.
    const EXPECT_A: &str = "<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>antiloki · MAN compliance</title><style>\n:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; }\nbody { font-family:-apple-system,BlinkMacSystemFont,system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }\n.wrap { max-width:1080px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.7em; margin:0 0 0.2em; }\nh2 { font-size:1.25em; margin-top:2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; }\n.meta { color:var(--muted); font-size:0.85em; margin:0.3em 0 1em; font-family:ui-monospace,monospace; }\n.hero { background:var(--panel); border:1px solid; border-radius:10px; padding:2em; text-align:center; margin:1em 0; }\n.hero.green  { border-color:var(--good); }\n.hero.yellow { border-color:var(--warn); }\n.hero.red    { border-color:var(--bad); }\n.hero .lamp { font-size:5em; line-height:1; }\n.hero .label { color:var(--muted); font-size:0.8em; font-family:ui-monospace,monospace; }\n.hero .sub   { color:var(--muted); font-size:0.95em; }\ntable { width:100%; border-collapse:collapse; margin:1em 0; font-size:0.92em; }\nth,td { padding:0.55em 0.8em; text-align:left; border-bottom:1px solid var(--border); }\nth { color:var(--muted); background:var(--panel); font-weight:500; font-size:0.85em; }\ncode { font-family:ui-monospace,monospace; font-size:0.88em; background:rgba(0,0,0,0.3); padding:1px 5px; border-radius:3px; }\n.cov-bar { display:inline-block; height:6px; background:var(--panel); border-radius:3px; width:80px; overflow:hidden; vertical-align:middle; margin-left:0.4em; }\n.cov-bar > div { height:100%; background:var(--good); }\n.tag { padding:1px 7px; border-radius:10px; font-size:0.78em; font-family:ui-monospace,monospace; }\n.tag.ok   { background:rgba(63,185,80,0.15); color:var(--good); border:1px solid rgba(63,185,80,0.4); }\n.tag.warn { background:rgba(210,153,34,0.15); color:var(--warn); border:1px solid rgba(210,153,34,0.4); }\n.tag.bad  { background:rgba(248,81,73,0.15); color:var(--bad); border:1px solid rgba(248,81,73,0.4); }\n.muted { color:var(--muted); }\n</style></head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"./index.html\" style=\"color:#58a6ff;text-decoration:none;\">← antiloki docs</a></nav><div class=\"wrap\">\n<div class=\"kicker\">antiloki-v3 · MAN compliance dashboard</div>\n<h1>Substrate health — drift + QA roll-up</h1>\n<p class=\"meta\">Window: <code>all</code> · regen: <code>node bin/antiloki.mjs man.render.compliance.html '{}' &gt; docs/man-compliance.html</code></p>\n\n<div class=\"hero red\">\n  <div class=\"lamp\">🔴</div>\n  <div class=\"label\">overall_health</div>\n  <div class=\"sub\">3 agents · mean coverage 50% · total drift: 4 unauthorized + 2 skipped</div>\n  <p class=\"muted\">worst offender: <code>a3</code> (drift_count=4)</p>\n</div>\n\n<h2>Per-agent compliance</h2>\n<table>\n  <tr><th>agent</th><th>coverage</th><th>unauthorized</th><th>skipped</th><th>summary</th></tr>\n  <tr>\n      <td><code>a1</code></td>\n      <td>100%<span class=\"cov-bar\"><div style=\"width:100%\"></div></span></td>\n      <td><span class=\"tag ok\">0</span></td>\n      <td><span class=\"tag ok\">0</span></td>\n      <td class=\"muted\">No drift detected.</td>\n    </tr><tr>\n      <td><code>a2</code></td>\n      <td>50%<span class=\"cov-bar\"><div style=\"width:50%\"></div></span></td>\n      <td><span class=\"tag warn\">1</span></td>\n      <td><span class=\"tag warn\">1</span></td>\n      <td class=\"muted\">Drift: 1 unauthorized actions, 1 skipped always-actions.</td>\n    </tr><tr>\n      <td><code>a3</code></td>\n      <td>0%<span class=\"cov-bar\"><div style=\"width:0%\"></div></span></td>\n      <td><span class=\"tag bad\">3</span></td>\n      <td><span class=\"tag bad\">1</span></td>\n      <td class=\"muted\">Drift: 3 unauthorized actions, 1 skipped always-actions.</td>\n    </tr>\n</table>\n\n<h2>Recent QA runs (last 10)</h2>\n<table>\n  <tr><th>contract</th><th>ts</th><th>verdict</th><th>AC pass/total</th><th>verifier</th></tr>\n  \n      <tr>\n        <td><code>c-two</code></td>\n        <td class=\"muted\">2026-07-21T08:00</td>\n        <td><span class=\"tag bad\">fail</span></td>\n        <td>1/3</td>\n        <td class=\"muted\"><code>human:dani</code></td>\n      </tr>\n      <tr>\n        <td><code>dev&amp;&lt;script&gt;&quot;</code></td>\n        <td class=\"muted\">2026-07-20T10:30</td>\n        <td><span class=\"tag ok\">pass</span></td>\n        <td>3/3</td>\n        <td class=\"muted\"><code>agent:be</code></td>\n      </tr>\n</table>\n</div></body></html>";

    const EXPECT_B: &str = "<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>antiloki · MAN compliance</title><style>\n:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; }\nbody { font-family:-apple-system,BlinkMacSystemFont,system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }\n.wrap { max-width:1080px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.7em; margin:0 0 0.2em; }\nh2 { font-size:1.25em; margin-top:2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; }\n.meta { color:var(--muted); font-size:0.85em; margin:0.3em 0 1em; font-family:ui-monospace,monospace; }\n.hero { background:var(--panel); border:1px solid; border-radius:10px; padding:2em; text-align:center; margin:1em 0; }\n.hero.green  { border-color:var(--good); }\n.hero.yellow { border-color:var(--warn); }\n.hero.red    { border-color:var(--bad); }\n.hero .lamp { font-size:5em; line-height:1; }\n.hero .label { color:var(--muted); font-size:0.8em; font-family:ui-monospace,monospace; }\n.hero .sub   { color:var(--muted); font-size:0.95em; }\ntable { width:100%; border-collapse:collapse; margin:1em 0; font-size:0.92em; }\nth,td { padding:0.55em 0.8em; text-align:left; border-bottom:1px solid var(--border); }\nth { color:var(--muted); background:var(--panel); font-weight:500; font-size:0.85em; }\ncode { font-family:ui-monospace,monospace; font-size:0.88em; background:rgba(0,0,0,0.3); padding:1px 5px; border-radius:3px; }\n.cov-bar { display:inline-block; height:6px; background:var(--panel); border-radius:3px; width:80px; overflow:hidden; vertical-align:middle; margin-left:0.4em; }\n.cov-bar > div { height:100%; background:var(--good); }\n.tag { padding:1px 7px; border-radius:10px; font-size:0.78em; font-family:ui-monospace,monospace; }\n.tag.ok   { background:rgba(63,185,80,0.15); color:var(--good); border:1px solid rgba(63,185,80,0.4); }\n.tag.warn { background:rgba(210,153,34,0.15); color:var(--warn); border:1px solid rgba(210,153,34,0.4); }\n.tag.bad  { background:rgba(248,81,73,0.15); color:var(--bad); border:1px solid rgba(248,81,73,0.4); }\n.muted { color:var(--muted); }\n</style></head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"./index.html\" style=\"color:#58a6ff;text-decoration:none;\">← antiloki docs</a></nav><div class=\"wrap\">\n<div class=\"kicker\">antiloki-v3 · MAN compliance dashboard</div>\n<h1>Substrate health — drift + QA roll-up</h1>\n<p class=\"meta\">Window: <code>all</code> · regen: <code>node bin/antiloki.mjs man.render.compliance.html '{}' &gt; docs/man-compliance.html</code></p>\n\n<div class=\"hero green\">\n  <div class=\"lamp\">🟢</div>\n  <div class=\"label\">overall_health</div>\n  <div class=\"sub\">0 agents · mean coverage 100% · total drift: 0 unauthorized + 0 skipped</div>\n  \n</div>\n\n<h2>Per-agent compliance</h2>\n<table>\n  <tr><th>agent</th><th>coverage</th><th>unauthorized</th><th>skipped</th><th>summary</th></tr>\n  \n</table>\n\n<h2>Recent QA runs (last 10)</h2>\n<table>\n  <tr><th>contract</th><th>ts</th><th>verdict</th><th>AC pass/total</th><th>verifier</th></tr>\n  <tr><td colspan=\"5\" class=\"muted\">No QA runs recorded yet.</td></tr>\n</table>\n</div></body></html>";

    #[test]
    fn full_dashboard_ok_warn_bad_and_qa_pass_fail() {
        let c = base_db();
        c.execute_batch(
            "INSERT INTO agent_stages VALUES (1,'a1','work',0),(2,'a2','work',0),(3,'a3','work',0); \
             INSERT INTO agent_stage_actions VALUES \
               (1,'a1.x','on_demand',0),(1,'a1.y','on_demand',1), \
               (2,'a2.x','always',0),(2,'a2.y','on_demand',1), \
               (3,'a3.x','always',0); \
             INSERT INTO runtime_events VALUES \
               (1,'t1','a1','a1.x','ok',5),(2,'t1','a1','a1.y','ok',5), \
               (3,'t1','a2','a2.y','ok',5),(4,'t1','a2','a2.rogue','ok',5), \
               (5,'t1','a3','a3.p','ok',5),(6,'t1','a3','a3.q','ok',5),(7,'t1','a3','a3.r','ok',5); \
             INSERT INTO qa_runs VALUES \
               (1,'dev&<script>\"','2026-07-20T10:30:00Z',1,3,0,'agent:be'), \
               (2,'c-two','2026-07-21T08:00:00Z',0,1,2,'human:dani');",
        ).unwrap();
        let r = manComplianceHtml(&json!({}), &c).unwrap();
        assert_eq!(r.as_str().unwrap(), EXPECT_A);
    }

    #[test]
    fn empty_substrate_green_no_qa() {
        let c = base_db();
        // null input exercises `input ?? {}`
        let r = manComplianceHtml(&Value::Null, &c).unwrap();
        assert_eq!(r.as_str().unwrap(), EXPECT_B);
    }

    #[test]
    fn helpers_match_js() {
        assert_eq!(to_fixed0(50.0), "50");
        assert_eq!(to_fixed0(0.0), "0");
        assert_eq!(to_fixed0(100.0), "100");
        assert_eq!(esc("dev&<script>\""), "dev&amp;&lt;script&gt;&quot;");
        assert_eq!(str_qq(Some(&Value::Null)), "");
    }
}
