//! agent-stages-html.rs — twin of skills/render/agent-stages-html.mjs (db READ lane). NAMES ARE
//! CONTRACT: `agentStagesHtml` (default). Static HTML reference of the canonical agent stages —
//! one section per agent, a table of stages with actions + invocation_pattern.
// Byte-identical template assembly: escHtml(&<>") · description.slice(0,100) · entry/exit ?? '—' ·
// order_index+1 · empty-actions colspan placeholder · agent/stage/action nesting joined with ''.
use rusqlite::Connection;
use serde_json::{json, Value};

// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
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
// escHtml = String(s ?? '').replace(/&/,'&amp;').replace(/</,'&lt;').replace(/>/,'&gt;').replace(/"/,'&quot;')
fn esc_html(v: Option<&Value>) -> String {
    str_qq(v)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// static template fragments (== the JS template literals; JS normalizes the file's \r\n to \n).
const PREFIX: &str = "<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>antiloki \u{b7} agent stages</title><style>\n:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; }\nbody { font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }\n.wrap { max-width:1200px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.7em; margin:0 0 0.2em; }\nh2 { font-size:1.3em; margin-top:2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); display:flex; align-items:baseline; gap:0.5em; }\nh3 { font-size:1.05em; margin-top:1.4em; color:var(--accent); }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; margin-bottom:0.2em; }\n.meta { color:var(--muted); font-size:0.85em; margin:0.3em 0 1em; font-family:ui-monospace,monospace; }\ntable { width:100%; border-collapse:collapse; margin:0.5em 0 1.2em; font-size:0.92em; }\nth,td { padding:0.55em 0.8em; text-align:left; border-bottom:1px solid var(--border); vertical-align:top; }\nth { color:var(--muted); background:var(--panel); font-weight:500; font-size:0.85em; }\ncode { font-family:ui-monospace,monospace; font-size:0.88em; background:rgba(0,0,0,0.3); padding:1px 5px; border-radius:3px; }\n.tag { display:inline-block; padding:1px 7px; border-radius:10px; font-size:0.78em; font-family:ui-monospace,monospace; }\n.tag.always      { background:rgba(63,185,80,0.15); color:var(--good); border:1px solid rgba(63,185,80,0.4); }\n.tag.conditional { background:rgba(210,153,34,0.15); color:var(--warn); border:1px solid rgba(210,153,34,0.4); }\n.tag.manual      { background:rgba(139,148,158,0.15); color:var(--muted); border:1px solid var(--border); }\n.agent { background:var(--panel); border:1px solid var(--border); border-radius:8px; padding:1em 1.2em; margin:0.8em 0; }\n.signals { color:var(--muted); font-size:0.85em; font-family:ui-monospace,monospace; margin:0.2em 0 0.6em; }\n.muted { color:var(--muted); }\n</style></head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"./index.html\" style=\"color:#58a6ff;text-decoration:none;\">\u{2190} antiloki docs</a></nav><div class=\"wrap\">\n<div class=\"kicker\">antiloki-v3 \u{b7} canonical agent stages</div>\n<h1>Agent workflow \u{2014} single source of truth</h1>\n<p class=\"meta\">Auto-generated from <code>agent_stages</code> + <code>agent_stage_actions</code>. Regen: <code>node bin/antiloki.mjs system.docs.agent_stages '{}'</code></p>\n";
const SUFFIX: &str = "\n</div></body></html>";
const SEC1: &str = "\n      <div class=\"agent\">\n        <h2><code>";
const SEC2: &str = "</code> <span class=\"muted\" style=\"font-size:0.7em;\">";
const SEC3: &str = "</span></h2>\n        ";
const SEC4: &str = "\n      </div>";
const PLACEHOLDER: &str = "<p class=\"muted\">no stages declared</p>";
const STG1: &str = "\n        <h3>";
const STG2: &str = ". ";
const STG3: &str = "</h3>\n        <p>";
const STG4: &str = "</p>\n        <div class=\"signals\">\n          entry: <code>";
const STG5: &str = "</code>\n          \u{2192} exit: <code>";
const STG6: &str = "</code>\n        </div>\n        <table>\n          <tr><th>action</th><th>invocation</th><th>rationale</th></tr>\n          ";
const STG7: &str = "\n        </table>";
const EMPTYROW: &str = "<tr><td colspan=\"3\" class=\"muted\">no actions bound yet (V1 placeholder)</td></tr>";
const ROW1: &str = "\n            <tr>\n              <td><code>";
const ROW2: &str = "</code></td>\n              <td><span class=\"tag ";
const ROW3: &str = "\">";
const ROW4: &str = "</span> ";
const OPT: &str = "<span class=\"muted\">optional</span>";
const ROW5: &str = "</td>\n              <td class=\"muted\">";
const ROW6: &str = "</td>\n            </tr>\n          ";

pub fn agentStagesHtml(_input: &Value, db: &Connection) -> Result<Value, String> {
    let agents = crate::db_json::query_rows(
        db,
        "SELECT DISTINCT a.name, a.description FROM agents a JOIN agent_stages s ON s.agent_name = a.name ORDER BY a.name",
        &[],
    )?;
    let agents = agents.as_array().cloned().unwrap_or_default();

    let mut sections = String::new();
    for a in &agents {
        let stages = crate::db_json::query_rows(
            db,
            "SELECT id, stage_name, order_index, description_md, entry_signal, exit_signal FROM agent_stages WHERE agent_name = ? ORDER BY order_index",
            &[a.get("name").cloned().unwrap_or(Value::Null)],
        )?;
        let stages = stages.as_array().cloned().unwrap_or_default();

        let mut stages_html = String::new();
        for s in &stages {
            let actions = crate::db_json::query_rows(
                db,
                "SELECT action_name, order_index, is_optional, invocation_pattern, rationale_md FROM agent_stage_actions WHERE agent_stage_id = ? ORDER BY order_index",
                &[s.get("id").cloned().unwrap_or(Value::Null)],
            )?;
            let actions = actions.as_array().cloned().unwrap_or_default();

            let skill_rows = if actions.is_empty() {
                EMPTYROW.to_string()
            } else {
                let mut rows = String::new();
                for sk in &actions {
                    let inv = esc_html(sk.get("invocation_pattern"));
                    rows.push_str(ROW1);
                    rows.push_str(&esc_html(sk.get("action_name")));
                    rows.push_str(ROW2);
                    rows.push_str(&inv);
                    rows.push_str(ROW3);
                    rows.push_str(&inv);
                    rows.push_str(ROW4);
                    rows.push_str(if truthy(sk.get("is_optional")) { OPT } else { "" });
                    rows.push_str(ROW5);
                    // rationale_md ?? '' — null coalesces to '' (str_qq of None is '' as well)
                    rows.push_str(&esc_html(sk.get("rationale_md").filter(|v| !v.is_null())));
                    rows.push_str(ROW6);
                }
                rows
            };

            // escHtml(s.order_index + 1) — order_index is an INTEGER column
            let oi = s.get("order_index").and_then(|v| v.as_i64()).unwrap_or(0);
            let label = esc_html(Some(&json!(oi + 1)));
            // s.entry_signal ?? '—' / s.exit_signal ?? '—' (— is U+2014)
            let entry = s.get("entry_signal").filter(|v| !v.is_null()).cloned().unwrap_or(json!("\u{2014}"));
            let exit = s.get("exit_signal").filter(|v| !v.is_null()).cloned().unwrap_or(json!("\u{2014}"));

            stages_html.push_str(STG1);
            stages_html.push_str(&label);
            stages_html.push_str(STG2);
            stages_html.push_str(&esc_html(s.get("stage_name")));
            stages_html.push_str(STG3);
            stages_html.push_str(&esc_html(s.get("description_md")));
            stages_html.push_str(STG4);
            stages_html.push_str(&esc_html(Some(&entry)));
            stages_html.push_str(STG5);
            stages_html.push_str(&esc_html(Some(&exit)));
            stages_html.push_str(STG6);
            stages_html.push_str(&skill_rows);
            stages_html.push_str(STG7);
        }

        // a.description?.slice(0, 100) ?? '' — only a string is sliced; null/absent → ''
        let desc = match a.get("description") {
            Some(Value::String(s)) => json!(s.chars().take(100).collect::<String>()),
            _ => json!(""),
        };

        sections.push_str(SEC1);
        sections.push_str(&esc_html(a.get("name")));
        sections.push_str(SEC2);
        sections.push_str(&esc_html(Some(&desc)));
        sections.push_str(SEC3);
        // stagesHtml || '<p ...>no stages declared</p>'
        if stages_html.is_empty() {
            sections.push_str(PLACEHOLDER);
        } else {
            sections.push_str(&stages_html);
        }
        sections.push_str(SEC4);
    }

    let mut out = String::with_capacity(PREFIX.len() + sections.len() + SUFFIX.len());
    out.push_str(PREFIX);
    out.push_str(&sections);
    out.push_str(SUFFIX);
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE agents (name TEXT, description TEXT); \
             CREATE TABLE agent_stages (id INTEGER PRIMARY KEY, agent_name TEXT, stage_name TEXT, order_index INTEGER, description_md TEXT, entry_signal TEXT, exit_signal TEXT); \
             CREATE TABLE agent_stage_actions (id INTEGER PRIMARY KEY, agent_stage_id INTEGER, action_name TEXT, order_index INTEGER, is_optional INTEGER, invocation_pattern TEXT, rationale_md TEXT); \
             INSERT INTO agents (name,description) VALUES ('beta',NULL),('alpha','Alpha <a> & \"b\"'); \
             INSERT INTO agent_stages (id,agent_name,stage_name,order_index,description_md,entry_signal,exit_signal) VALUES \
               (1,'alpha','Intake',0,'Collects <input>','user message','plan ready'), \
               (2,'alpha','Review & Confirm',1,'Confirm the plan',NULL,NULL), \
               (3,'beta','Solo',0,'Beta stage','signal','done'); \
             INSERT INTO agent_stage_actions (id,agent_stage_id,action_name,order_index,is_optional,invocation_pattern,rationale_md) VALUES \
               (1,1,'card.add',0,0,'always','always run'), \
               (2,1,'memory.add',1,1,'conditional',NULL), \
               (3,3,'test.run',0,0,'manual','manual step');",
        )
        .unwrap();
        c
    }

    #[test]
    fn renders() {
        let html = agentStagesHtml(&json!({}), &seed()).unwrap();
        let s = html.as_str().unwrap();
        // scaffolding + no CRLF
        assert!(s.starts_with("<!DOCTYPE html>\n<html>"));
        assert!(s.ends_with("\n</div></body></html>"));
        assert!(!s.contains('\r'));
        // the source's verbatim quirk is preserved byte-for-byte
        assert!(s.contains("BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif"));
        // ORDER BY a.name → alpha section precedes beta section
        let a = s.find("<code>alpha</code>").unwrap();
        let b = s.find("<code>beta</code>").unwrap();
        assert!(a < b);
        // escHtml over &, <, >, "
        assert!(s.contains("Alpha &lt;a&gt; &amp; &quot;b&quot;"));
        assert!(s.contains("<p>Collects &lt;input&gt;</p>"));
        // order_index + 1 rendering
        assert!(s.contains("<h3>1. Intake</h3>"));
        assert!(s.contains("<h3>2. Review &amp; Confirm</h3>"));
        // is_optional true → optional span; false → trailing space only
        assert!(s.contains("<span class=\"tag conditional\">conditional</span> <span class=\"muted\">optional</span>"));
        assert!(s.contains("<span class=\"tag always\">always</span> </td>"));
        // rationale_md ?? '' when NULL → empty cell
        assert!(s.contains("<td class=\"muted\"></td>"));
        // entry/exit ?? '—' (U+2014) on the NULL-signal stage
        assert!(s.contains("entry: <code>\u{2014}</code>"));
        assert!(s.contains("\u{2192} exit: <code>\u{2014}</code>"));
        // null description → empty span
        assert!(s.contains("<code>beta</code> <span class=\"muted\" style=\"font-size:0.7em;\"></span>"));
    }

    #[test]
    fn empty_actions_placeholder() {
        // stage 2 (alpha 'Review & Confirm') has no bound actions → colspan placeholder row
        let html = agentStagesHtml(&json!({}), &seed()).unwrap();
        assert!(html.as_str().unwrap().contains(
            "<tr><td colspan=\"3\" class=\"muted\">no actions bound yet (V1 placeholder)</td></tr>"
        ));
    }
}
