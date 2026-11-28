//! anatomy-html.rs — twin of skills/render/anatomy-html.mjs (db READ lane, cross-db:
//! memory.anatomy_snapshots). NAMES ARE CONTRACT: `anatomyHtml` (default). Renders the latest
//! anatomy snapshot as a collapsible HTML page (counts/tables/views/agents/actions/skills).
// The template literals are CRLF in source but JS normalizes template-literal line terminators to
// LF, so the byte-exact output uses "\n" only (same rule confirmed by render/memory-html.rs).

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
// String(v ?? '') — also Array.prototype.join's per-element coercion (undefined/null -> '').
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
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
// JS escapeHtml: & first, then <, >, " (order matters). Missing key (JS undefined) -> "undefined".
fn escape_html_str(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn escape_val(v: Option<&Value>) -> String {
    match v {
        None => escape_html_str("undefined"),
        Some(x) => escape_html_str(&js_string(x)),
    }
}
// `arr || []` — an array (even empty) is truthy in JS; only undefined/null fall through to [].
fn arr_or_default(v: Option<&Value>) -> Vec<Value> {
    if truthy(v) { v.and_then(|x| x.as_array()).cloned().unwrap_or_default() } else { Vec::new() }
}

const CSS: &str = "\n:root { --bg:#1e1e1e; --fg:#ddd; --muted:#888; --bar:#444; --card:#2a2a2a; --accent:#88ddff; --type:#ffaa66; }\nbody { font-family: 'JetBrains Mono', Consolas, monospace; padding: 1.5em; background: var(--bg); color: var(--fg); max-width: 1100px; margin: 0 auto; line-height: 1.5; }\nh1, h2 { color: #fff; font-weight: 400; }\nh2 { border-bottom: 1px solid var(--bar); padding-bottom: 0.3em; margin-top: 2em; }\n.counts { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 0.5em; margin: 1em 0; }\n.count { background: var(--card); padding: 0.7em 1em; border-radius: 4px; }\n.count .label { color: var(--muted); font-size: 0.85em; }\n.count .num { color: var(--accent); font-size: 1.6em; font-weight: 600; }\ntable { width: 100%; border-collapse: collapse; margin: 0.5em 0; background: var(--card); border-radius: 4px; overflow: hidden; }\nth, td { padding: 0.5em 1em; text-align: left; border-bottom: 1px solid var(--bar); }\nth { color: var(--muted); font-weight: 500; font-size: 0.85em; }\ndetails { background: var(--card); margin: 0.4em 0; border-radius: 4px; padding: 0.5em 1em; }\nsummary { cursor: pointer; color: #fff; }\nsummary code { color: var(--accent); }\n.muted { color: var(--muted); }\n.list-inline code { background: #3a3a3a; padding: 1px 5px; border-radius: 2px; margin-right: 0.3em; font-size: 0.9em; color: var(--accent); }\n";

// The full <body><nav>…</nav> line, verbatim (contains a source typo `<body>quot;` for `&quot;`).
const NAV: &str = r#"<body><nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>"#;

fn render_counts(counts: &Value) -> String {
    let cells: String = match counts.as_object() {
        Some(obj) => obj
            .iter()
            .map(|(k, v)| format!("<div class=\"count\"><div class=\"label\">{}</div><div class=\"num\">{}</div></div>", escape_html_str(k), tmpl(Some(v))))
            .collect::<Vec<_>>()
            .join(""),
        None => String::new(),
    };
    format!("<div class=\"counts\">{cells}</div>")
}

fn render_tables(tables: &[Value]) -> String {
    tables
        .iter()
        .map(|t| {
            let columns = t.get("columns").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let rows: String = columns
                .iter()
                .map(|c| {
                    let pk = if truthy(c.get("pk")) { "PK " } else { "" };
                    let notnull = if truthy(c.get("notnull")) { "NOT NULL" } else { "" };
                    format!("<tr><td>{}</td><td>{}</td><td class=\"muted\">{pk}{notnull}</td></tr>", escape_val(c.get("name")), escape_val(c.get("type")))
                })
                .collect::<Vec<_>>()
                .join("");
            format!(
                "\n<details>\n  <summary><code>{}</code> <span class=\"muted\">({} columns)</span></summary>\n  <table>\n    <tr><th>column</th><th>type</th><th>flags</th></tr>\n    {rows}\n  </table>\n</details>",
                escape_val(t.get("name")),
                columns.len()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_agents(agents: &[Value]) -> String {
    let rows: String = agents
        .iter()
        .map(|a| {
            format!(
                "<tr><td><code>{}</code></td><td>{}</td><td class=\"muted\">{}</td></tr>",
                escape_val(a.get("name")),
                tmpl(a.get("skill_count")),
                escape_val(a.get("description"))
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!("<table><tr><th>name</th><th>action count</th><th>description</th></tr>{rows}</table>")
}

// Renders `payload.actions` — the JS name `renderSkills` is a pre-existing misnomer, kept only as
// a comment (this internal helper isn't a unit; NAMES ARE CONTRACT only binds the default export).
fn render_actions_table(actions: &[Value]) -> String {
    let rows: String = actions
        .iter()
        .map(|s| {
            let desc = if truthy(s.get("description")) { js_string(s.get("description").unwrap()) } else { String::new() };
            format!(
                "<tr><td><code>{}</code></td><td>{}</td><td class=\"muted\">{}</td></tr>",
                escape_val(s.get("name")),
                escape_val(s.get("owner_agent")),
                escape_html_str(&desc)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!("<table><tr><th>name</th><th>owner</th><th>description</th></tr>{rows}</table>")
}

fn render_subskills(skills: &[Value]) -> String {
    skills
        .iter()
        .map(|s| {
            let default_val = s.get("default");
            let def = if truthy(default_val) {
                let d = default_val.unwrap();
                let params = arr_or_default(d.get("params"));
                let params_joined = params.iter().map(|p| str_qq(Some(p))).collect::<Vec<_>>().join(", ");
                format!("<code>default: {}fn({params_joined})</code>", if truthy(d.get("async")) { "async " } else { "" })
            } else {
                "<span class=\"muted\">no default export</span>".to_string()
            };
            let imports = s.get("imports").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let import_list = if imports.is_empty() {
                "<span class=\"muted\">(none)</span>".to_string()
            } else {
                format!(
                    "<div class=\"list-inline\">{}</div>",
                    imports.iter().map(|i| format!("<code>{}</code>", escape_val(Some(i)))).collect::<Vec<_>>().join("")
                )
            };
            let exports = s.get("exports").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let export_list = if exports.is_empty() {
                "<span class=\"muted\">(none)</span>".to_string()
            } else {
                format!(
                    "<div class=\"list-inline\">{}</div>",
                    exports.iter().map(|e| format!("<code>{}</code>", escape_val(e.get("name")))).collect::<Vec<_>>().join("")
                )
            };
            format!(
                "\n<details>\n  <summary><code>{}</code> <span class=\"muted\">({} lines)</span></summary>\n  <p>{def}</p>\n  <p><span class=\"muted\">imports:</span> {import_list}</p>\n  <p><span class=\"muted\">named exports:</span> {export_list}</p>\n  <p class=\"muted\" style=\"font-size:0.8em\">sha256: {}</p>\n</details>",
                escape_val(s.get("name")),
                tmpl(s.get("lines")),
                escape_val(s.get("sha256"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn anatomyHtml(_input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('anatomy-html','db') when no db — the db lane always injects one.
    let rows = crate::db_json::query_rows(db, "SELECT * FROM memory.anatomy_snapshots ORDER BY id DESC LIMIT 1", &[])?;
    let latest = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => {
            return Ok(Value::String(format!(
                "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>Anatomy</title><style>{CSS}</style></head>\n{NAV}<h1>Anatomy</h1><p class=\"muted\">No snapshots yet. Run system.anatomy.snapshot first.</p></body></html>"
            )));
        }
    };

    let payload_json = latest.get("payload_json").and_then(|v| v.as_str()).unwrap_or("");
    let payload: Value = serde_json::from_str(payload_json).map_err(|e| e.to_string())?;

    let id = tmpl(latest.get("id"));
    let taken_at = escape_val(latest.get("taken_at"));
    let taken_by = escape_val(latest.get("taken_by"));
    let git_sha = latest.get("git_sha");
    let git_suffix = if truthy(git_sha) {
        let short: String = js_string(git_sha.unwrap()).chars().take(7).collect();
        format!(" · git {}", escape_html_str(&short))
    } else {
        String::new()
    };

    let counts = payload.get("counts").cloned().unwrap_or(Value::Object(serde_json::Map::new()));
    let tables = payload.get("tables").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let views = payload.get("views").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let agents = payload.get("agents").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let actions = payload.get("actions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let skills = payload.get("skills").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let views_joined = views.iter().map(|v| format!("<code>{}</code>", escape_val(Some(v)))).collect::<Vec<_>>().join(", ");

    let html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Anatomy · #{id}</title>\n<style>{CSS}</style>\n</head>\n{NAV}\n<h1>Anatomy · snapshot #{id}</h1>\n<p class=\"muted\">taken at {taken_at} by {taken_by}{git_suffix}</p>\n{}\n<h2>Tables ({}) + Views ({})</h2>\n{}\n<p>Views: {views_joined}</p>\n<h2>Agents ({})</h2>\n{}\n<h2>Actions ({})</h2>\n{}\n<h2>Skills ({})</h2>\n{}\n</body>\n</html>",
        render_counts(&counts),
        tables.len(),
        views.len(),
        render_tables(&tables),
        agents.len(),
        render_agents(&agents),
        actions.len(),
        render_actions_table(&actions),
        skills.len(),
        render_subskills(&skills),
    );

    Ok(Value::String(html))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; \
             CREATE TABLE memory.anatomy_snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, taken_at TEXT NOT NULL DEFAULT (datetime('now')), payload_json TEXT NOT NULL, sha256 TEXT NOT NULL UNIQUE, taken_by TEXT NOT NULL, git_sha TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn renders_empty_state_with_no_snapshots() {
        let c = conn();
        let got = anatomyHtml(&json!({}), &c).unwrap();
        let s = got.as_str().unwrap();
        assert!(s.starts_with("<!DOCTYPE html>"));
        assert!(s.contains("No snapshots yet. Run system.anatomy.snapshot first."));
    }

    #[test]
    fn renders_full_snapshot_with_escaping_and_git_sha() {
        let c = conn();
        let payload = json!({
            "counts": {"cards": 3, "memories": 7},
            "tables": [{"name": "cards", "columns": [
                {"name": "id", "type": "INTEGER", "pk": true, "notnull": true},
                {"name": "title", "type": "TEXT", "pk": false, "notnull": false}
            ]}],
            "views": ["v_open_cards"],
            "agents": [{"name": "dev", "skill_count": 4, "description": "builds things"}],
            "actions": [
                {"name": "card.add", "owner_agent": "dev", "description": "adds a card"},
                {"name": "with-audit", "owner_agent": "system"}
            ],
            "skills": [
                {"name": "card-add", "lines": 42, "default": {"async": false, "params": ["input", "context"]}, "imports": ["../engine/error.mjs", "../engine/open-db.mjs"], "exports": [{"name": "helperOne"}], "sha256": "abc123"},
                {"name": "with-audit", "lines": 10, "imports": [], "exports": [], "sha256": "def456"}
            ]
        });
        c.execute(
            "INSERT INTO memory.anatomy_snapshots (id, taken_at, payload_json, sha256, taken_by, git_sha) VALUES (1, '2025-12-01 00:00:00', ?, 'h1', ?, NULL)",
            rusqlite::params![payload.to_string(), "tester"],
        )
        .unwrap();
        let got = anatomyHtml(&json!({}), &c).unwrap();
        let s = got.as_str().unwrap();
        assert!(s.contains("Anatomy · snapshot #1"));
        assert!(s.contains("<code>cards</code>"));
        assert!(s.contains("PK NOT NULL"));
        assert!(s.contains("<td class=\"muted\"></td></tr>")); // title: not pk, not notnull -> both flags empty
        assert!(s.contains("<code>v_open_cards</code>"));
        assert!(s.contains("<code>default: fn(input, context)</code>"));
        assert!(s.contains("no default export"));
        assert!(s.contains("(none)")); // with-audit has empty imports/exports
        // with-audit's OWN row (owner=system, no description key -> muted cell empty). The prior
        // pattern here checked card.add's trailing muted cell instead, which is non-empty ("adds a
        // card") and never matches — a copy-paste of the columns-table pattern above, not adapted
        // for the actions table's 3-column row shape (name, owner, description).
        assert!(s.contains("<tr><td><code>with-audit</code></td><td>system</td><td class=\"muted\"></td></tr>"));
        assert!(!s.contains(" · git ")); // git_sha NULL -> no suffix
    }

    #[test]
    fn escapes_html_and_renders_async_default_with_empty_params_and_git_sha() {
        let c = conn();
        let payload = json!({
            "counts": {"cards": 0},
            "tables": [],
            "views": ["v_a", "v_b"],
            "agents": [],
            "actions": [],
            "skills": [
                {"name": "async-skill", "lines": 5, "default": {"async": true, "params": []}, "imports": ["x.mjs"], "exports": [], "sha256": "zzz"}
            ]
        });
        c.execute(
            "INSERT INTO memory.anatomy_snapshots (id, taken_at, payload_json, sha256, taken_by, git_sha) VALUES (1, '2025-12-01 00:00:00', ?, 'h1', '<script>x</script>', 'abcdef1234567')",
            rusqlite::params![payload.to_string()],
        )
        .unwrap();
        let got = anatomyHtml(&json!({}), &c).unwrap();
        let s = got.as_str().unwrap();
        assert!(s.contains("&lt;script&gt;x&lt;/script&gt;"));
        assert!(!s.contains("<script>x"));
        assert!(s.contains(" · git abcdef1"));
        assert!(s.contains("<code>default: async fn()</code>"));
        assert!(s.contains("Views: <code>v_a</code>, <code>v_b</code>"));
        assert!(s.contains("Tables (0) + Views (2)"));
    }
}
