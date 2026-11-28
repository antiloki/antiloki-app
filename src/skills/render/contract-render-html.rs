//! contract-render-html.rs — twin of skills/render/contract-render-html.mjs (db READ lane).
//! NAMES ARE CONTRACT: `contractRenderHtml` (named default). Renders one persisted contract as a
//! self-contained HTML page; high-risk + tl_approved shows the HITL approval banner. Reads the
//! contract via the contract-get twin, plus sacred_timeline phase events (entity_type='contract').
// QUALITY: byte-exact HTML — escHtml = String(s).replace(...); CSS/nav verbatim (incl. the
// `<body>quot;` source bug); JSON.stringify(ac,null,0)=compact, JSON.stringify(naming,null,2)=pretty.
use rusqlite::Connection;
use serde_json::{json, Value};

// JS String(v): null→"null", array→join(","), object→"[object Object]".
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
// JS String(v ?? '') for scalar `${v}` interpolation.
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `${v}` — absent→"undefined", null→"null", else scalar string.
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
// JS escHtml(s) = String(s).replace(...) — undefined→"undefined", null→"null".
fn esch(v: Option<&Value>) -> String {
    let s = match v { None => "undefined".to_string(), Some(x) => js_string(x) };
    esc(&s)
}
// JS `x ?? []` then treated as array (null/undefined/non-array → empty).
fn arr(v: Option<&Value>) -> Vec<Value> {
    nz(v).and_then(|x| x.as_array()).cloned().unwrap_or_default()
}

const CSS: &str = "\n:root {\n  --bg:#0e1116; --panel:#161b22; --panel2:#1f2530; --border:#30363d;\n  --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950;\n  --warn:#d29922; --bad:#f85149; --code-bg:#0d1117;\n}\n* { box-sizing: border-box; }\nbody { font-family: -apple-system, BlinkMacSystemFont, \"Segoe UI\", system-ui, sans-serif; background: var(--bg); color: var(--fg); margin: 0; padding: 0; line-height: 1.55; }\n.wrap { max-width: 1080px; margin: 0 auto; padding: 2em 1.5em 4em; }\nh1 { font-size: 1.7em; font-weight: 700; margin: 0 0 0.2em; }\nh2 { font-size: 1.25em; font-weight: 600; margin-top: 2.2em; padding-bottom: 0.3em; border-bottom: 1px solid var(--border); }\n.kicker { color: var(--accent); font-family: ui-monospace, monospace; font-size: 0.78em; text-transform: uppercase; letter-spacing: 0.08em; margin-bottom: 0.2em; }\n.meta { color: var(--muted); font-size: 0.85em; margin: 0.3em 0 1em; font-family: ui-monospace, monospace; }\ncode, pre { font-family: ui-monospace, \"JetBrains Mono\", Consolas, monospace; font-size: 0.88em; }\ncode { background: var(--code-bg); padding: 1px 5px; border-radius: 3px; border: 1px solid var(--border); }\npre { background: var(--code-bg); padding: 0.9em 1.1em; border-radius: 6px; border: 1px solid var(--border); overflow-x: auto; }\ntable { width: 100%; border-collapse: collapse; margin: 0.5em 0 1.2em; font-size: 0.92em; }\nth, td { padding: 0.6em 0.9em; text-align: left; border-bottom: 1px solid var(--border); vertical-align: top; }\nth { color: var(--muted); font-weight: 500; font-size: 0.85em; background: var(--panel); }\n.badges { display: flex; gap: 0.5em; flex-wrap: wrap; margin: 0.4em 0 1em; }\n.badge { display: inline-block; padding: 3px 9px; border-radius: 10px; font-size: 0.8em; font-family: ui-monospace, monospace; font-weight: 500; }\n.badge.tier-trivial    { background: rgba(63,185,80,0.12);  color: var(--good); border: 1px solid rgba(63,185,80,0.4); }\n.badge.tier-standard   { background: rgba(88,166,255,0.12); color: var(--accent); border: 1px solid rgba(88,166,255,0.4); }\n.badge.tier-high-risk  { background: rgba(248,81,73,0.12);  color: var(--bad); border: 1px solid rgba(248,81,73,0.4); }\n.badge.status-proposed       { background: rgba(139,148,158,0.12); color: var(--muted); border: 1px solid var(--border); }\n.badge.status-tl-approved    { background: rgba(210,153,34,0.12);  color: var(--warn); border: 1px solid rgba(210,153,34,0.4); }\n.badge.status-human-approved { background: rgba(63,185,80,0.12);   color: var(--good); border: 1px solid rgba(63,185,80,0.4); }\n.badge.status-impl    { background: rgba(88,166,255,0.12); color: var(--accent); border: 1px solid rgba(88,166,255,0.4); }\n.badge.status-qa      { background: rgba(188,140,255,0.12); color: #bc8cff; border: 1px solid rgba(188,140,255,0.4); }\n.badge.status-done    { background: rgba(63,185,80,0.18); color: var(--good); border: 1px solid rgba(63,185,80,0.5); }\n.badge.status-rejected,.badge.status-superseded { background: rgba(248,81,73,0.12); color: var(--bad); border: 1px solid rgba(248,81,73,0.4); }\n.badge.size,.badge.size-XS,.badge.size-S,.badge.size-M,.badge.size-L { background: var(--panel); border: 1px solid var(--border); color: var(--fg); }\n.panel { background: var(--panel); border: 1px solid var(--border); border-radius: 8px; padding: 1.1em 1.3em; margin: 1em 0; }\n.hitl { background: rgba(248,81,73,0.08); border-color: var(--bad); }\n.hitl h2 { border: 0; margin-top: 0; color: var(--bad); }\n.muted { color: var(--muted); }\nul { padding-left: 1.4em; }\nli { margin: 0.25em 0; }\n.ac-kind { color: var(--accent); font-family: ui-monospace, monospace; font-size: 0.85em; font-weight: 600; }\nhr { border: 0; border-top: 1px solid var(--border); margin: 2.5em 0; }\n";

const NAV: &str = "<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"../index.html\" style=\"color:#58a6ff;text-decoration:none;\">\u{2190} antiloki docs</a></nav>";

fn render_ac_list(ac_items: &[Value]) -> String {
    if ac_items.is_empty() {
        return "<p class=\"muted\">No acceptance criteria yet \u{2014} TL must add at least one.</p>".to_string();
    }
    let inner: String = ac_items.iter().enumerate().map(|(i, ac)| {
        let compact = serde_json::to_string(ac).unwrap_or_default();
        let sliced: String = compact.chars().take(300).collect();
        let mut s = String::new();
        s.push_str("\n      <tr>\n        <td class=\"muted\">");
        s.push_str(&(i + 1).to_string());
        s.push_str("</td>\n        <td><span class=\"ac-kind\">");
        s.push_str(&esch(ac.get("kind")));
        s.push_str("</span></td>\n        <td><pre style=\"margin:0\">");
        s.push_str(&esc(&sliced));
        s.push_str("</pre></td>\n      </tr>\n    ");
        s
    }).collect();
    let mut out = String::new();
    out.push_str("<table>\n    <tr><th>#</th><th>kind</th><th>spec</th></tr>\n    ");
    out.push_str(&inner);
    out.push_str("\n  </table>");
    out
}

fn render_hitl_banner(slug_e: &str) -> String {
    let mut s = String::new();
    s.push_str("\n    <div class=\"panel hitl\">\n      <h2>\u{26a0} Awaiting human approval (high-risk tier)</h2>\n      <p>This contract is signed by TL but its tier requires explicit human acknowledgement before <code>impl</code> opens. The score is NOT penalized for this gate.</p>\n      <p><strong>Approve via:</strong></p>\n      <pre><code>node bin/antiloki.mjs contract.gate.human.approve '{\"slug\":\"");
    s.push_str(slug_e);
    s.push_str("\"}'</code></pre>\n      <p><strong>Reject via:</strong></p>\n      <pre><code>node bin/antiloki.mjs contract.gate.human.reject '{\"slug\":\"");
    s.push_str(slug_e);
    s.push_str("\",\"reason\":\"...\"}'</code></pre>\n      <p class=\"muted\">Actions <code>contract.gate.human.*</code> are themselves backlog cards \u{2014} for now, override by inserting a phase event manually (HIGH-RISK FALLBACK ONLY).</p>\n    </div>\n  ");
    s
}

pub fn contractRenderHtml(input: &Value, db: &Connection) -> Result<Value, String> {
    // slug = typeof input === 'string' ? input : input?.slug
    let slug = if let Some(s) = input.as_str() {
        s.to_string()
    } else {
        input.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    if slug.is_empty() {
        return Err("contract-render-html: slug is required".into());
    }

    let contract = crate::contract_contract_get::contractGet(&json!(slug), db)?;
    if contract.is_null() {
        return Err(format!("contract '{}' not found", slug));
    }

    // Phase events (try/catch → empty on error, e.g. table missing).
    let phase_events = crate::db_json::query_rows(
        db,
        "SELECT id, ts, actor, operation, payload FROM sacred_timeline WHERE entity_type = 'contract' AND entity_id = ? ORDER BY id ASC",
        &[json!(slug)],
    ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();

    let tier = contract.get("tier").and_then(|v| v.as_str()).unwrap_or("standard").to_string();
    let is_high_risk = tier == "high-risk";
    let status = contract.get("status").and_then(|v| v.as_str()).unwrap_or("proposed").to_string();
    let show_hitl = is_high_risk && status == "tl_approved";

    let none = json!("none");
    let qmark = json!("?");
    let one = json!(1);

    let slug_e = esch(contract.get("slug"));

    // headerSection
    let mut header = String::new();
    header.push_str("\n    <div class=\"kicker\">contract</div>\n    <h1>");
    header.push_str(&esch(contract.get("title")));
    header.push_str("</h1>\n    <div class=\"meta\"><code>");
    header.push_str(&slug_e);
    header.push_str("</code> \u{b7} card: <code>");
    header.push_str(&esch(Some(nz(contract.get("card_slug")).unwrap_or(&none))));
    header.push_str("</code> \u{b7} template: <code>");
    header.push_str(&esch(Some(nz(contract.get("template_kind")).unwrap_or(&none))));
    header.push_str("</code></div>\n    <div class=\"badges\">\n      <span class=\"badge status-");
    header.push_str(&status.replace('_', "-"));
    header.push_str("\">");
    header.push_str(&esc(&status));
    header.push_str("</span>\n      <span class=\"badge tier-");
    header.push_str(&tier.replace('_', "-"));
    header.push_str("\">");
    header.push_str(&esc(&tier));
    header.push_str("</span>\n      ");
    if truthy(contract.get("size")) {
        header.push_str("<span class=\"badge size-");
        header.push_str(&esch(contract.get("size")));
        header.push_str("\">size: ");
        header.push_str(&esch(contract.get("size")));
        header.push_str("</span>");
    }
    header.push_str("\n      ");
    if nz(contract.get("risk")).is_some() {
        header.push_str("<span class=\"badge size\">risk: ");
        header.push_str(&tmpl(contract.get("risk")));
        header.push_str("/5</span>");
    }
    header.push_str("\n      ");
    if nz(contract.get("complexity")).is_some() {
        header.push_str("<span class=\"badge size\">complexity: ");
        header.push_str(&tmpl(contract.get("complexity")));
        header.push_str("/5</span>");
    }
    header.push_str("\n      <span class=\"badge size\">attempt ");
    header.push_str(&tmpl(Some(nz(contract.get("current_attempt")).unwrap_or(&one))));
    header.push_str("</span>\n    </div>\n  ");

    // sigSection
    let sig = if truthy(contract.get("signed_by")) {
        let mut s = String::new();
        s.push_str("<div class=\"panel\"><strong>Signature:</strong> <code>");
        s.push_str(&esch(contract.get("signed_by")));
        s.push_str("</code> at <code>");
        s.push_str(&esch(Some(nz(contract.get("signed_at")).unwrap_or(&qmark))));
        s.push_str("</code></div>");
        s
    } else {
        "<div class=\"panel\"><strong>Not yet signed by TL.</strong></div>".to_string()
    };

    // tlVoiceSection
    let tlvoice = if truthy(contract.get("tl_briefing_md")) || truthy(contract.get("tl_evaluation_md")) {
        let mut s = String::new();
        s.push_str("<h2>TL voice</h2>\n       ");
        if truthy(contract.get("tl_briefing_md")) {
            s.push_str("<h3>Briefing \u{2014} card intent \u{2192} contract</h3><pre>");
            s.push_str(&esch(contract.get("tl_briefing_md")));
            s.push_str("</pre>");
        }
        s.push_str("\n       ");
        if truthy(contract.get("tl_evaluation_md")) {
            s.push_str("<h3>Evaluation \u{2014} template + risks</h3><pre>");
            s.push_str(&esch(contract.get("tl_evaluation_md")));
            s.push_str("</pre>");
        }
        s
    } else {
        String::new()
    };

    let hitl = if show_hitl { render_hitl_banner(&slug_e) } else { String::new() };

    // acceptanceSection
    let acceptance_arr = arr(contract.get("acceptance"));
    let mut acceptance = String::new();
    acceptance.push_str("\n    <h2>Acceptance criteria (");
    acceptance.push_str(&acceptance_arr.len().to_string());
    acceptance.push_str(")</h2>\n    ");
    acceptance.push_str(&render_ac_list(&acceptance_arr));
    acceptance.push_str("\n  ");

    // filesSection
    let files_arr = arr(contract.get("files_touched"));
    let mut files = String::new();
    files.push_str("\n    <h2>Files touched (");
    files.push_str(&files_arr.len().to_string());
    files.push_str(")</h2>\n    ");
    if files_arr.is_empty() {
        files.push_str("<p class=\"muted\">none declared</p>");
    } else {
        files.push_str("<ul>");
        for p in &files_arr {
            files.push_str("<li><code>");
            files.push_str(&esch(Some(p)));
            files.push_str("</code></li>");
        }
        files.push_str("</ul>");
    }
    files.push_str("\n  ");

    // namingSection
    let naming_v = nz(contract.get("naming")).cloned().unwrap_or_else(|| json!({}));
    let naming_pretty = serde_json::to_string_pretty(&naming_v).unwrap_or_default();
    let mut naming = String::new();
    naming.push_str("\n    <h2>Naming</h2>\n    <pre><code>");
    naming.push_str(&esc(&naming_pretty));
    naming.push_str("</code></pre>\n  ");

    // edgeSection
    let edge_arr = arr(contract.get("edge_cases"));
    let mut edge = String::new();
    edge.push_str("\n    <h2>Edge cases (");
    edge.push_str(&edge_arr.len().to_string());
    edge.push_str(")</h2>\n    ");
    if edge_arr.is_empty() {
        edge.push_str("<p class=\"muted\">none enumerated</p>");
    } else {
        edge.push_str("<table><tr><th>when</th><th>then</th></tr>");
        for e in &edge_arr {
            edge.push_str("<tr><td><code>");
            edge.push_str(&esch(Some(nz(e.get("when")).unwrap_or(&qmark))));
            edge.push_str("</code></td><td>");
            edge.push_str(&esch(Some(nz(e.get("then")).unwrap_or(&qmark))));
            edge.push_str("</td></tr>");
        }
        edge.push_str("</table>");
    }
    edge.push_str("\n  ");

    // oosSection
    let oos_arr = arr(contract.get("out_of_scope"));
    let mut oos = String::new();
    oos.push_str("\n    <h2>Out of scope (");
    oos.push_str(&oos_arr.len().to_string());
    oos.push_str(")</h2>\n    <ul>");
    for o in &oos_arr {
        oos.push_str("<li class=\"muted\">");
        oos.push_str(&esch(Some(o)));
        oos.push_str("</li>");
    }
    oos.push_str("</ul>\n  ");

    // lawsSection
    let laws_arr = arr(contract.get("must_obey"));
    let mut laws = String::new();
    laws.push_str("\n    <h2>Must obey (");
    laws.push_str(&laws_arr.len().to_string());
    laws.push_str(" laws)</h2>\n    <ul>");
    for l in &laws_arr {
        laws.push_str("<li><code>");
        laws.push_str(&esch(Some(l)));
        laws.push_str("</code></li>");
    }
    laws.push_str("</ul>\n  ");

    // refSection
    let ref_arr = arr(contract.get("reference_artifacts"));
    let mut refsec = String::new();
    refsec.push_str("\n    <h2>Reference artifacts (");
    refsec.push_str(&ref_arr.len().to_string());
    refsec.push_str(")</h2>\n    <ul>");
    for r in &ref_arr {
        refsec.push_str("<li><code>");
        refsec.push_str(&esch(Some(r)));
        refsec.push_str("</code></li>");
    }
    refsec.push_str("</ul>\n  ");

    // briefingSection
    let briefing = if truthy(contract.get("briefing_md")) {
        let mut s = String::new();
        s.push_str("<h2>Briefing</h2><pre>");
        s.push_str(&esch(contract.get("briefing_md")));
        s.push_str("</pre>");
        s
    } else {
        String::new()
    };

    // phasesSection
    let mut phases = String::new();
    phases.push_str("\n    <h2>Phase events (sacred_timeline)</h2>\n    ");
    if phase_events.is_empty() {
        phases.push_str("<p class=\"muted\">no audit events recorded for this contract yet</p>");
    } else {
        phases.push_str("<table><tr><th>ts</th><th>actor</th><th>operation</th></tr>");
        for e in &phase_events {
            phases.push_str("<tr><td class=\"muted\">");
            phases.push_str(&esch(e.get("ts")));
            phases.push_str("</td><td><code>");
            phases.push_str(&esch(e.get("actor")));
            phases.push_str("</code></td><td><code>");
            phases.push_str(&esch(e.get("operation")));
            phases.push_str("</code></td></tr>");
        }
        phases.push_str("</table>");
    }
    phases.push_str("\n  ");

    // final document
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"UTF-8\">\n<title>contract \u{b7} ");
    out.push_str(&slug_e);
    out.push_str("</title>\n<style>");
    out.push_str(CSS);
    out.push_str("</style>\n</head>\n");
    out.push_str(NAV);
    out.push_str("\n<div class=\"wrap\">\n");
    out.push_str(&header); out.push('\n');
    out.push_str(&sig); out.push('\n');
    out.push_str(&tlvoice); out.push('\n');
    out.push_str(&hitl); out.push('\n');
    out.push_str(&acceptance); out.push('\n');
    out.push_str(&files); out.push('\n');
    out.push_str(&naming); out.push('\n');
    out.push_str(&edge); out.push('\n');
    out.push_str(&oos); out.push('\n');
    out.push_str(&laws); out.push('\n');
    out.push_str(&refsec); out.push('\n');
    out.push_str(&briefing); out.push('\n');
    out.push_str(&phases); out.push('\n');
    out.push_str("<hr>\n<p class=\"muted\" style=\"text-align:center\">Auto-generated from <code>contracts</code> table \u{b7} regen: <code>node bin/antiloki.mjs contract.render.html '{\"slug\":\"");
    out.push_str(&slug_e);
    out.push_str("\"}'</code></p>\n</div>\n</body>\n</html>");

    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT, title TEXT, card_slug TEXT, template_kind TEXT, status TEXT, tier TEXT, size TEXT, risk INTEGER, complexity INTEGER, current_attempt INTEGER, signed_by TEXT, signed_at TEXT, tl_briefing_md TEXT, tl_evaluation_md TEXT, briefing_md TEXT, acceptance_json TEXT, files_touched_json TEXT, naming_json TEXT, edge_cases_json TEXT, out_of_scope_json TEXT, must_obey_json TEXT, reference_artifacts_json TEXT, generated_paths_json TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, ts TEXT, actor TEXT, operation TEXT, payload TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn high_risk_full_render_with_hitl() {
        let c = conn();
        c.execute_batch(
            "INSERT INTO contracts (slug,title,card_slug,template_kind,status,tier,size,risk,complexity,current_attempt,signed_by,signed_at,tl_briefing_md,tl_evaluation_md,briefing_md,acceptance_json,files_touched_json,naming_json,edge_cases_json,out_of_scope_json,must_obey_json,reference_artifacts_json,generated_paths_json) \
             VALUES ('CT-1','Ship <it> & \"go\"','CARD-9','feature','tl_approved','high-risk','M',4,3,2,'agent:tl','2026-01-01T00:00:00Z','brief <md>','eval md','main brief','[{\"kind\":\"test\",\"spec\":\"login\"}]','[\"src/a.js\",\"src/b.js\"]','{\"fn\":\"doIt\",\"file\":\"x.js\"}','[{\"when\":\"empty\",\"then\":\"error\"}]','[\"telemetry\"]','[\"LAW_X\"]','[\"docs/y.md\"]','[]'); \
             INSERT INTO sacred_timeline (entity_type,entity_id,ts,actor,operation) VALUES ('contract','CT-1','2026-01-02T00:00:00Z','human:dani','contract.sign');",
        ).unwrap();
        let r = contractRenderHtml(&json!("CT-1"), &c).unwrap();
        let s = r.as_str().unwrap();

        assert!(s.starts_with("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"UTF-8\">\n<title>contract \u{b7} CT-1</title>"));
        assert!(s.contains("<body>quot;Segoe UI<body>quot;")); // verbatim source bug preserved
        assert!(s.contains("<h1>Ship &lt;it&gt; &amp; &quot;go&quot;</h1>")); // escaped title
        assert!(s.contains("<span class=\"badge status-tl-approved\">tl_approved</span>"));
        assert!(s.contains("<span class=\"badge tier-high-risk\">high-risk</span>"));
        assert!(s.contains("<span class=\"badge size-M\">size: M</span>"));
        assert!(s.contains("<span class=\"badge size\">risk: 4/5</span>"));
        assert!(s.contains("<span class=\"badge size\">complexity: 3/5</span>"));
        assert!(s.contains("<span class=\"badge size\">attempt 2</span>"));
        // HITL banner (high-risk + tl_approved)
        assert!(s.contains("<h2>\u{26a0} Awaiting human approval (high-risk tier)</h2>"));
        assert!(s.contains("contract.gate.human.approve '{\"slug\":\"CT-1\"}'"));
        // TL voice with em-dash / arrow
        assert!(s.contains("<h3>Briefing \u{2014} card intent \u{2192} contract</h3><pre>brief &lt;md&gt;</pre>"));
        // acceptance table: JSON.stringify(ac,null,0) compact + escaped, escHtml(kind)
        assert!(s.contains("<span class=\"ac-kind\">test</span>"));
        assert!(s.contains("<pre style=\"margin:0\">{&quot;kind&quot;:&quot;test&quot;,&quot;spec&quot;:&quot;login&quot;}</pre>"));
        assert!(s.contains("<h2>Acceptance criteria (1)</h2>"));
        assert!(s.contains("<h2>Files touched (2)</h2>"));
        assert!(s.contains("<li><code>src/a.js</code></li><li><code>src/b.js</code></li>"));
        // naming pretty-printed (2-space) then escaped
        assert!(s.contains("<pre><code>{\n  &quot;fn&quot;: &quot;doIt&quot;,\n  &quot;file&quot;: &quot;x.js&quot;\n}</code></pre>"));
        assert!(s.contains("<tr><td><code>empty</code></td><td>error</td></tr>"));
        assert!(s.contains("<li class=\"muted\">telemetry</li>"));
        assert!(s.contains("<h2>Must obey (1 laws)</h2>"));
        assert!(s.contains("<li><code>LAW_X</code></li>"));
        assert!(s.contains("<li><code>docs/y.md</code></li>"));
        assert!(s.contains("<h2>Briefing</h2><pre>main brief</pre>"));
        // phase events table
        assert!(s.contains("<tr><td class=\"muted\">2026-01-02T00:00:00Z</td><td><code>human:dani</code></td><td><code>contract.sign</code></td></tr>"));
        assert!(s.ends_with("contract.render.html '{\"slug\":\"CT-1\"}'</code></p>\n</div>\n</body>\n</html>"));
    }

    #[test]
    fn standard_minimal_no_hitl_empty_sections() {
        let c = conn();
        // unsigned, standard tier, null status → 'proposed'; NULL json cols → empty; no events
        c.execute_batch(
            "INSERT INTO contracts (slug,title,status,tier,current_attempt) VALUES ('CT-2','Plain',NULL,NULL,NULL);",
        ).unwrap();
        let r = contractRenderHtml(&json!({ "slug": "CT-2" }), &c).unwrap();
        let s = r.as_str().unwrap();

        assert!(s.contains("<span class=\"badge status-proposed\">proposed</span>"));
        assert!(s.contains("<span class=\"badge tier-standard\">standard</span>"));
        assert!(s.contains("<span class=\"badge size\">attempt 1</span>")); // ?? 1
        assert!(!s.contains("class=\"badge size-")); // no size badge
        assert!(!s.contains("risk:")); // risk null → omitted
        assert!(!s.contains("\u{26a0} Awaiting human approval")); // no HITL
        assert!(s.contains("<div class=\"panel\"><strong>Not yet signed by TL.</strong></div>"));
        assert!(s.contains("card: <code>none</code>")); // card_slug ?? 'none'
        assert!(s.contains("<p class=\"muted\">No acceptance criteria yet \u{2014} TL must add at least one.</p>"));
        assert!(s.contains("<p class=\"muted\">none declared</p>"));
        assert!(s.contains("<pre><code>{}</code></pre>")); // naming ?? {}
        assert!(s.contains("<p class=\"muted\">none enumerated</p>"));
        assert!(s.contains("<p class=\"muted\">no audit events recorded for this contract yet</p>"));
        // no TL voice section when both md fields absent
        assert!(!s.contains("<h2>TL voice</h2>"));
    }

    #[test]
    fn missing_slug_and_not_found_err() {
        let c = conn();
        assert!(contractRenderHtml(&json!({}), &c).is_err());
        assert!(contractRenderHtml(&json!("GHOST"), &c).is_err());
    }
}
