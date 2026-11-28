//! docs-index-html.rs — twin of skills/render/docs-index-html.mjs (db READ lane).
//! NAMES ARE CONTRACT: `docsIndexHtml` (default). Dashboard of every HTML-producing action, read
//! from state.actions and grouped by name-prefix; grouping order is the fixed GROUP_ORDER, not row
//! order. OUTPUT_MAP is a static path/note lookup keyed by action name (not DB-derived).
use rusqlite::Connection;
use serde_json::Value;

// JS escHtml: String(s ?? '').replace(/&/,'&amp;').replace(/</,'&lt;').replace(/>/,'&gt;').replace(/"/,'&quot;')
// — callers resolve the `?? ''` themselves before calling, so this takes an already-`&str` value.
fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

// OUTPUT_MAP: (action name, fixed output path, note). Static data — NOT derived from the db.
const OUTPUT_MAP: &[(&str, Option<&str>, Option<&str>)] = &[
    ("board-keeper.card.html", None, Some("per-card · output path is caller-supplied")),
    ("card.render.html", None, Some("per-card · docs/cards/<slug>.html")),
    ("card.render.index.html", Some("cards/index.html"), Some("board of all cards")),
    ("contract.render.html", None, Some("per-contract · docs/contracts/<slug>.html")),
    ("contract.render.index.html", Some("contracts/index.html"), Some("board of all contracts")),
    ("man.render.compliance.html", Some("man-compliance.html"), None),
    ("system.anatomy.html", Some("anatomy.html"), Some("requires a snapshot first")),
    ("system.ast.html", None, Some("per-skill · caller-supplied path")),
    ("system.docs.agent_stages", Some("agent-stages.html"), None),
    ("system.docs.defense", Some("substrate-defense.html"), Some("5 enforcement layers · fail-closed hard-law hooks · 16-dimension scan")),
    ("system.docs.driver", Some("driver.html"), Some("state machine + tick lifecycle + sliced QA-fail retries")),
    ("system.docs.sandbox", Some("sandbox.html"), Some("copy-on-write filesystem sandbox")),
    ("system.docs.getting_started", Some("getting-started.html"), Some("★ start here — guided tour for new readers")),
    ("system.docs.concurrency", Some("concurrency.html"), Some("WAL mode, safe parallel ops, backup/restore")),
    ("system.docs.rag", Some("rag-flow.html"), Some("embeddings + the ai-call pipeline (structured output, two-phase, tiers)")),
    ("system.log.tail.html", Some("runtime.html"), Some("auto-refreshing")),
    ("system.memory.graph.html", Some("memory-graph.html"), None),
    ("system.memory.html", Some("memory.html"), None),
    ("system.score.autonomy.html", Some("autonomy-score.html"), None),
];

fn output_map(name: &str) -> Option<(Option<&'static str>, Option<&'static str>)> {
    OUTPUT_MAP.iter().find(|(n, _, _)| *n == name).map(|(_, p, note)| (*p, *note))
}

// Group label per action prefix.
fn group_of(name: &str) -> &'static str {
    if name.starts_with("board-keeper.") || name.starts_with("card.") { return "Board · cards"; }
    if name.starts_with("contract.") { return "Contracts"; }
    if name.starts_with("man.") { return "Compliance"; }
    if name.starts_with("system.anatomy") { return "System · anatomy"; }
    if name.starts_with("system.memory") { return "Memory"; }
    if name.starts_with("system.score") { return "Score"; }
    if name.starts_with("system.docs") { return "Reference docs"; }
    if name.starts_with("system.log") { return "Runtime · logs"; }
    if name.starts_with("system.ast") { return "Audit · introspection"; }
    "Other"
}

const GROUP_ORDER: &[&str] = &[
    "Reference docs",
    "Board · cards",
    "Contracts",
    "Compliance",
    "Memory",
    "System · anatomy",
    "Score",
    "Runtime · logs",
    "Audit · introspection",
    "Other",
];

// CSS constant — byte-exact value of the JS `const CSS` template (leading + trailing "\n").
const CSS: &str = "\n:root { --bg:#0e1116; --panel:#161b22; --panel2:#1f2530; --border:#30363d;\n        --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; }\n* { box-sizing:border-box; }\nbody { font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }\n.wrap { max-width:1080px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.9em; font-weight:700; letter-spacing:-0.02em; margin:0 0 0.2em; }\nh2 { font-size:1.15em; font-weight:600; margin-top:2.2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); color:var(--fg); }\n.lede { color:var(--muted); font-size:1.02em; max-width:680px; }\n.meta { color:var(--muted); font-size:0.82em; margin:0.3em 0 1.6em; font-family:ui-monospace,monospace; }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; margin-bottom:0.2em; }\ncode { font-family:ui-monospace,\"JetBrains Mono\",Consolas,monospace; font-size:0.85em; background:rgba(0,0,0,0.3); padding:1px 5px; border-radius:3px; border:1px solid var(--border); }\n.grid { display:grid; grid-template-columns:repeat(auto-fit,minmax(330px,1fr)); gap:0.7em; margin-top:0.6em; }\n.card { background:var(--panel); border:1px solid var(--border); border-radius:6px; padding:0.85em 1.1em; transition:border-color 0.12s, background 0.12s; }\n.card.live { border-color:rgba(88,166,255,0.4); }\n.card.live:hover { border-color:var(--accent); background:var(--panel2); }\n.card .title { display:flex; align-items:baseline; justify-content:space-between; gap:0.6em; }\n.card .title a { color:var(--accent); text-decoration:none; font-weight:600; font-size:1.02em; }\n.card .title a:hover { text-decoration:underline; }\n.card .title .name { color:var(--muted); font-family:ui-monospace,monospace; font-size:0.78em; }\n.card .desc { color:var(--fg); font-size:0.88em; margin:0.4em 0 0.5em; }\n.card .note { color:var(--muted); font-size:0.78em; margin:0.2em 0 0.4em; font-style:italic; }\n.card .regen { color:var(--muted); font-size:0.78em; font-family:ui-monospace,monospace; word-break:break-all; }\n.card.dyn { opacity:0.78; border-style:dashed; }\n.legend { color:var(--muted); font-size:0.82em; margin:0.4em 0 0; }\nhr { border:0; border-top:1px solid var(--border); margin:2em 0; }\n";

pub fn docsIndexHtml(_input: &Value, db: &Connection) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT name, description FROM actions WHERE name LIKE '%.html' OR name LIKE '%.index.html' OR name LIKE 'system.docs.%' ORDER BY name",
        &[],
    )?;
    let rows = rows.as_array().cloned().unwrap_or_default();

    // byGroup: Map key order is irrelevant (only .has()/.get() are used) — the render order below
    // follows GROUP_ORDER, not insertion order.
    let mut by_group: std::collections::HashMap<&'static str, Vec<Value>> = std::collections::HashMap::new();
    for r in &rows {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        by_group.entry(group_of(name)).or_default().push(r.clone());
    }
    let empty: Vec<Value> = Vec::new();

    let groups_html: String = GROUP_ORDER
        .iter()
        .copied()
        .filter(|g| by_group.contains_key(g))
        .map(|g| {
            let items: String = by_group
                .get(g)
                .unwrap_or(&empty)
                .iter()
                .map(|row| {
                    let name = row.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let desc = row.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    let (path, note) = output_map(name).unwrap_or((None, None));
                    let live = path.is_some();
                    let title_line = match path {
                        Some(p) => format!("<a href=\"./{p}\">{p}</a>", p = esc_html(p)),
                        None => format!(
                            "<span class=\"title-disabled\" style=\"color:var(--muted);\">{}</span>",
                            esc_html(note.unwrap_or("(per-entity render)"))
                        ),
                    };
                    let note_line = if live {
                        note.map(|n| format!("<div class=\"note\">{}</div>", esc_html(n))).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let regen = match path {
                        Some(p) => format!("node bin/antiloki.mjs {} '{{}}' &gt; docs/{}", esc_html(name), esc_html(p)),
                        None => format!("node bin/antiloki.mjs {} '{{\u{2026}}}'", esc_html(name)),
                    };
                    format!(
                        "<div class=\"card {cls}\">\n          <div class=\"title\">{title}<span class=\"name\">{ename}</span></div>\n          <div class=\"desc\">{edesc}</div>\n          {note_line}\n          <div class=\"regen\">{regen}</div>\n        </div>",
                        cls = if live { "live" } else { "dyn" },
                        title = title_line,
                        ename = esc_html(name),
                        edesc = esc_html(desc),
                        note_line = note_line,
                        regen = regen,
                    )
                })
                .collect::<Vec<_>>()
                .join("");
            format!("<h2>{}</h2><div class=\"grid\">{}</div>", esc_html(g), items)
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Frozen clock (conform lane): new Date().toISOString() → this literal.
    let generated_at = crate::clock::now_iso_nodb();
    let total_active = OUTPUT_MAP.iter().filter(|(_, p, _)| p.is_some()).count();
    let total_dynamic = OUTPUT_MAP.iter().filter(|(_, p, _)| p.is_none()).count();

    let lines: [String; 13] = [
        "<!DOCTYPE html>".to_string(),
        format!("<html><head><meta charset=\"UTF-8\"><title>antiloki · docs index</title><style>{}</style></head>", CSS),
        "<body><div class=\"wrap\">".to_string(),
        "<div class=\"kicker\">antiloki-v3 · docs</div>".to_string(),
        "<h1>antiloki HTML renders</h1>".to_string(),
        "<p class=\"lede\">All HTML-producing actions, grouped by domain. Links point to the canonical output path under <code>docs/</code>. Dashed cards are per-entity renders (caller picks the path).</p>".to_string(),
        format!(
            "<p class=\"meta\">Generated <code>{}</code> · {} actions · {} fixed-path + {} dynamic · regen <code>node bin/antiloki.mjs system.docs.index '{{}}' &gt; docs/index.html</code></p>",
            esc_html(&generated_at), rows.len(), total_active, total_dynamic
        ),
        String::new(),
        groups_html,
        String::new(),
        "<hr>".to_string(),
        "<p class=\"legend\">Source of truth: <code>SELECT name, description FROM actions WHERE name LIKE '%.html' OR name LIKE 'system.docs.%' ORDER BY name</code>. Path mapping in <code>.antiloki/skills/render/docs-index-html.mjs</code>.</p>".to_string(),
        "</div></body></html>".to_string(),
    ];
    Ok(Value::String(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seed(rows: &[(&str, Option<&str>)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE actions (name TEXT, description TEXT)").unwrap();
        for (name, desc) in rows {
            c.execute("INSERT INTO actions (name, description) VALUES (?1, ?2)", rusqlite::params![name, desc]).unwrap();
        }
        c
    }
    fn html(rows: &[(&str, Option<&str>)]) -> String {
        match docsIndexHtml(&json!({}), &seed(rows)).unwrap() {
            Value::String(s) => s,
            _ => unreachable!(),
        }
    }

    #[test]
    fn empty_actions_renders_shell_with_static_counts() {
        let s = html(&[]);
        assert!(s.starts_with("<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>antiloki · docs index</title><style>"));
        assert!(s.ends_with("</div></body></html>"));
        assert!(s.contains("· 0 actions · 15 fixed-path + 4 dynamic · regen"));
        assert!(!s.contains("<h2>"));
    }

    #[test]
    fn groups_render_in_fixed_group_order_not_row_order() {
        let s = html(&[
            ("system.score.autonomy.html", Some("Score")),
            ("system.docs.rag", Some("RAG docs")),
            ("board-keeper.card.html", Some("per-card")),
        ]);
        let i_docs = s.find("<h2>Reference docs</h2>").unwrap();
        let i_board = s.find("<h2>Board · cards</h2>").unwrap();
        let i_score = s.find("<h2>Score</h2>").unwrap();
        assert!(i_docs < i_board && i_board < i_score);
        assert!(!s.contains("<h2>Contracts</h2>"));
    }

    #[test]
    fn output_map_hit_with_fixed_path_renders_live_link_and_note() {
        let s = html(&[("card.render.index.html", Some("Board of all cards"))]);
        assert!(s.contains("<div class=\"card live\">"));
        assert!(s.contains("<div class=\"title\"><a href=\"./cards/index.html\">cards/index.html</a><span class=\"name\">card.render.index.html</span></div>"));
        assert!(s.contains("<div class=\"desc\">Board of all cards</div>"));
        assert!(s.contains("<div class=\"note\">board of all cards</div>"));
        assert!(s.contains("<div class=\"regen\">node bin/antiloki.mjs card.render.index.html '{}' &gt; docs/cards/index.html</div>"));
    }

    #[test]
    fn output_map_hit_fixed_path_no_note_omits_note_div() {
        let s = html(&[("man.render.compliance.html", Some("Compliance page"))]);
        assert!(s.contains("<div class=\"card live\">"));
        assert!(!s.contains("<div class=\"note\">"));
    }

    #[test]
    fn output_map_hit_null_path_uses_note_as_title_dyn_card_dynamic_regen() {
        let s = html(&[("board-keeper.card.html", Some("per-card render"))]);
        assert!(s.contains("<div class=\"card dyn\">"));
        assert!(s.contains("<span class=\"title-disabled\" style=\"color:var(--muted);\">per-card · output path is caller-supplied</span>"));
        assert!(!s.contains("<div class=\"note\">"));
        assert!(s.contains("<div class=\"regen\">node bin/antiloki.mjs board-keeper.card.html '{\u{2026}}'</div>"));
    }

    #[test]
    fn unmapped_action_falls_back_to_per_entity_render_in_other_group() {
        let s = html(&[("zzz.unmapped.html", Some("Something else entirely"))]);
        assert!(s.contains("<h2>Other</h2>"));
        assert!(s.contains("<span class=\"title-disabled\" style=\"color:var(--muted);\">(per-entity render)</span>"));
        assert!(s.contains("<div class=\"card dyn\">"));
    }

    #[test]
    fn null_description_renders_empty_desc() {
        let s = html(&[("contract.render.index.html", None)]);
        assert!(s.contains("<div class=\"desc\"></div>"));
    }

    #[test]
    fn description_html_metacharacters_are_escaped() {
        let s = html(&[("system.memory.html", Some("A & B <b>\"bold\"</b>"))]);
        assert!(s.contains("<div class=\"desc\">A &amp; B &lt;b&gt;&quot;bold&quot;&lt;/b&gt;</div>"));
    }

    #[test]
    fn rows_outside_where_filter_are_excluded_by_the_sql_itself() {
        let c = seed(&[]);
        c.execute(
            "INSERT INTO actions (name, description) VALUES ('man.approve.contract', 'not a doc action'), ('system.docs.rag', 'RAG docs')",
            [],
        )
        .unwrap();
        let s = match docsIndexHtml(&json!({}), &c).unwrap() { Value::String(s) => s, _ => unreachable!() };
        assert!(!s.contains("man.approve.contract"));
        assert!(s.contains("system.docs.rag"));
        assert!(s.contains("· 1 actions ·"));
    }

    #[test]
    fn multiple_items_in_same_group_concatenate_in_sql_order() {
        let s = html(&[("system.docs.driver", Some("Driver docs")), ("system.docs.rag", Some("RAG docs"))]);
        let grid_start = s.find("<h2>Reference docs</h2><div class=\"grid\">").unwrap();
        let driver_idx = s[grid_start..].find("system.docs.driver").unwrap();
        let rag_idx = s[grid_start..].find("system.docs.rag").unwrap();
        assert!(driver_idx < rag_idx);
    }

    #[test]
    fn deterministic() {
        let rows = [("system.docs.rag", Some("RAG docs"))];
        assert_eq!(html(&rows), html(&rows));
    }
}
