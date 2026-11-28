//! getting-started-html.rs — twin of skills/render/getting-started-html.mjs (standard lane; pure docs, no db).
//! NAMES ARE CONTRACT: `gettingStartedHtml` (default). Renders the "where to start" landing page.
// Static output: the JS body is a single template literal with no branches and only two
// interpolations (CSS, NAV) — same value for every call, input/context are ignored. Template
// literals normalize CRLF/CR line terminators to LF (ECMA-262 TRV rule; verified against the
// source file, which is CRLF on disk), so the twin joins line pieces with explicit "\n" and never
// embeds a physical newline, matching render/sandbox-doc-html.rs's proven pattern.
use rusqlite::Connection;
use serde_json::Value;

// CSS constant — byte-exact value of the JS `const CSS` template (leading + trailing "\n").
const CSS: &str = "\n:root { --bg:#0e1116; --panel:#161b22; --panel2:#1f2530; --border:#30363d;\n        --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; }\n* { box-sizing:border-box; }\nbody { font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.6; }\n.wrap { max-width:780px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:2em; font-weight:700; margin:0 0 0.2em; }\nh2 { font-size:1.3em; font-weight:600; margin-top:2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); }\nh3 { font-size:1.02em; font-weight:600; margin-top:1.4em; }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; margin-bottom:0.2em; }\n.lede { color:var(--muted); font-size:1.05em; max-width:680px; }\n.meta { color:var(--muted); font-size:0.82em; font-family:ui-monospace,monospace; }\ncode, pre { font-family:ui-monospace,\"JetBrains Mono\",Consolas,monospace; font-size:0.86em; }\ncode { background:#0d1117; padding:1px 5px; border-radius:3px; border:1px solid var(--border); }\npre { background:#0d1117; padding:0.9em 1.1em; border-radius:6px; border:1px solid var(--border); overflow-x:auto; }\n.callout { background:var(--panel); border:1px solid var(--accent); border-radius:6px; padding:1em 1.2em; margin:1em 0; }\n.callout-title { font-weight:600; color:var(--accent); margin-bottom:0.4em; font-size:0.92em; text-transform:uppercase; letter-spacing:0.06em; }\na { color:var(--accent); }\n.tour { display:grid; grid-template-columns:140px 1fr; gap:0.8em 1em; margin:1em 0; }\n.tour .step { color:var(--muted); font-family:ui-monospace,monospace; font-size:0.85em; padding-top:0.2em; }\n.tour .what { padding-bottom:0.6em; border-bottom:1px solid var(--border); }\n.tour .what:last-child { border-bottom:0; }\nul { padding-left:1.4em; }\nli { margin:0.3em 0; }\n";

// NAV constant — byte-exact value of the JS `const NAV` template (single line, verbatim).
const NAV: &str = r#"<nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,&quot;Segoe UI&quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>"#;

pub fn gettingStartedHtml(_input: &Value, _db: &Connection) -> Result<Value, String> {
    let lines: Vec<String> = vec![
        r#"<!DOCTYPE html>"#.to_string(),
        [r#"<html><head><meta charset="UTF-8"><title>antiloki · getting started</title><style>"#, CSS, r#"</style></head>"#].concat(),
        [r#"<body>"#, NAV, r#"<div class="wrap">"#].concat(),
        r#"<div class="kicker">antiloki-v3</div>"#.to_string(),
        r#"<h1>Where to start</h1>"#.to_string(),
        r#"<p class="lede">antiloki is a deterministic substrate for AI-driven software work. It tracks cards through PL → TL → DEV → QA stages, enforces laws, captures audit trails, and provides a copy-on-write filesystem sandbox so DEV's mistakes never touch your project files until QA approves.</p>"#.to_string(),
        r#""#.to_string(),
        r#"<div class="callout">"#.to_string(),
        r#"  <div class="callout-title">The 5-minute mental model</div>"#.to_string(),
        r#"  Everything is a <strong>SQLite row</strong> or a <strong>skill</strong> (atomic <code>.mjs</code> file). <strong>Actions</strong> compose skills declaratively. <strong>Agents</strong> own actions. <strong>Laws</strong> enforce invariants at runtime. <strong>The driver</strong> walks cards through the workflow without manual dispatch. <strong>The sandbox</strong> protects your originals."#.to_string(),
        r#"</div>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>Pick your starting point</h2>"#.to_string(),
        r#""#.to_string(),
        r#"<h3>I want to see it work end-to-end</h3>"#.to_string(),
        r#"<p>Two runnable concept proofs (each <2s, no API keys needed):</p>"#.to_string(),
        r#"<pre><code># Driver: state machine, scheduler, tick, escalation"#.to_string(),
        r#"node bin/driver-demo.mjs"#.to_string(),
        r#""#.to_string(),
        r#"# Sandbox: TL creates sandbox, DEV writes copy, QA promotes atomically,"#.to_string(),
        r#"# tamper detection demonstrated, reset path demonstrated"#.to_string(),
        r#"node bin/sandbox-demo.mjs</code></pre>"#.to_string(),
        r#""#.to_string(),
        r#"<h3>I want a live dashboard</h3>"#.to_string(),
        r#"<pre><code>npm run tui          # 4-pane watch screen, refreshes every 1s"#.to_string(),
        r#"                     # Press 'd' to toggle the driver-state pane</code></pre>"#.to_string(),
        r#""#.to_string(),
        r#"<h3>I want to read the architecture</h3>"#.to_string(),
        r#"<div class="tour">"#.to_string(),
        r#"  <div class="step">1. start here</div>"#.to_string(),
        r#"  <div class="what"><a href="./substrate-defense.html">substrate-defense.html</a> — the 5-DB layout + 5 enforcement layers + how the substrate resists AI hallucination</div>"#.to_string(),
        r#"  <div class="step">2. agents + stages</div>"#.to_string(),
        r#"  <div class="what"><a href="./agent-stages.html">agent-stages.html</a> — the 10 agents (system, board-keeper, pl, tl, dev, qa, sec, man, god, in) + their declared stages</div>"#.to_string(),
        r#"  <div class="step">3. orchestration</div>"#.to_string(),
        r#"  <div class="what"><a href="./driver.html">driver.html</a> — state machine rule table + tick lifecycle + budget caps + escape hatches</div>"#.to_string(),
        r#"  <div class="step">4. file safety</div>"#.to_string(),
        r#"  <div class="what"><a href="./sandbox.html">sandbox.html</a> — copy-on-write filesystem sandbox: lifecycle, tamper detection, atomic promote</div>"#.to_string(),
        r#"  <div class="step">5. laws</div>"#.to_string(),
        r#"  <div class="what"><a href="../AGENTS.md">../AGENTS.md</a> — the 34 active laws (auto-generated from <code>laws.sqlite</code>; this is what AGENTS.md imports)</div>"#.to_string(),
        r#"  <div class="step">6. RAG</div>"#.to_string(),
        r#"  <div class="what"><a href="./rag-flow.html">rag-flow.html</a> — memory layer (embeddings, graph links, retrieval) + how the substrate learns from its own corrections</div>"#.to_string(),
        r#"</div>"#.to_string(),
        r#""#.to_string(),
        r#"<h3>I want to USE it from Claude Code</h3>"#.to_string(),
        r#"<p>Already wired. <code>.mcp.json</code> at repo root configures antiloki as an MCP server. Restart Claude Code in this directory and you'll see 149 actions as tools (<code>board_keeper_card_add</code>, <code>system_driver_tick</code>, <code>system_memory_rag_context</code>, …).</p>"#.to_string(),
        r#"<p>For SAFE mode (no raw freelance): once <code>FEAT-INTERVIEWER-GATE</code> ships, set <code>ANTILOKI_INTERVIEWER_ONLY=1</code> in <code>.mcp.json</code>'s env and the MCP server will expose ONLY <code>interviewer_turn</code>.</p>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>Key principles</h2>"#.to_string(),
        r#"<ul>"#.to_string(),
        r#"  <li><strong>Determinism over LLMs</strong> — the state machine, scheduler, escalation, scope-check, acceptance-run, and audit chain are all pure code. The only AI step in the QA path is zero. The only AI step in the DEV path is <code>dev.implement.code</code>, guarded by output schema validation + path whitelist.</li>"#.to_string(),
        r#"  <li><strong>Audit-anchored</strong> — every mutation goes through <code>with-audit</code> and lands in <code>sacred_timeline</code> with a Merkle-chained <code>digital_print</code>. Tamper detection is cryptographic.</li>"#.to_string(),
        r#"  <li><strong>Laws fire at runtime</strong> — not just CI lint. A law violation can <strong>block</strong> a dispatch in flight. Verified live: <code>COMMIT_CODE_BEFORE_AUDIT_COMMIT</code> caught real drift mid-session multiple times.</li>"#.to_string(),
        r#"  <li><strong>5 DBs, 5 concerns</strong> — state (what IS), memory (what EXPERIENCED), brain (what THINKS), laws (what MUST), knowledge (what LEARNED). No 6th DB unless a new orthogonal concern shows up.</li>"#.to_string(),
        r#"</ul>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>If you find yourself…</h2>"#.to_string(),
        r#"<ul>"#.to_string(),
        r#"  <li><strong>looking for a specific HTML render</strong> → <a href="./index.html">docs/index.html</a> (the dashboard)</li>"#.to_string(),
        r#"  <li><strong>looking for a specific action by name</strong> → <code>SELECT name, description FROM actions WHERE name LIKE '%X%'</code> against <code>.antiloki/db/state.sqlite</code></li>"#.to_string(),
        r#"  <li><strong>looking for a learned pattern (correction, lesson)</strong> → <code>system.memory.rag.context</code> with a query vector, or <code>SELECT * FROM memory.memories WHERE type IN ('correction','lesson')</code></li>"#.to_string(),
        r#"  <li><strong>looking for what the driver did</strong> → <code>SELECT * FROM brain.driver_ticks ORDER BY id DESC LIMIT 20</code></li>"#.to_string(),
        r#"  <li><strong>about to add a file to a contract</strong> → confirm it's in <code>files_touched</code> first; the sandbox enforces scope at write-time</li>"#.to_string(),
        r#"</ul>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>Open epics worth knowing about</h2>"#.to_string(),
        r#"<ul>"#.to_string(),
        r#"  <li><code>FEAT-INTERVIEWER-GATE</code> — sole conversational entry; closes the "raw Claude freelance" gap</li>"#.to_string(),
        r#"  <li><code>FEAT-AI-PROVIDER-CLAUDE-CODE</code> — route <code>ai-call</code> through Claude Code (Max sub) instead of Anthropic API credits</li>"#.to_string(),
        r#"  <li><code>FEAT-PROD-READINESS-WAL-BACKUP</code> — WAL mode + backup + concurrency for multi-process use</li>"#.to_string(),
        r#"</ul>"#.to_string(),
        r#""#.to_string(),
        r#"<p class="meta">Regen: <code>node bin/antiloki.mjs system.docs.getting_started '{}' &gt; docs/getting-started.html</code></p>"#.to_string(),
        r#"</div></body></html>"#.to_string(),
    ];
    Ok(Value::String(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn out() -> String {
        match gettingStartedHtml(&json!({}), &Connection::open_in_memory().unwrap()).unwrap() {
            Value::String(s) => s,
            _ => unreachable!(),
        }
    }
    #[test]
    fn byte_exact_structure() {
        let s = out();
        assert!(s.starts_with("<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>antiloki · getting started</title><style>\n:root {"));
        assert!(s.ends_with("</div></body></html>"));
        assert!(!s.contains('\r'), "output must be LF-only");
        assert!(s.contains("← antiloki docs</a></nav>"));
        assert!(s.contains("<h1>Where to start</h1>"));
        assert!(s.contains("<h2>Pick your starting point</h2>"));
        assert!(s.contains("<h2>Key principles</h2>"));
        assert!(s.contains("<h2>If you find yourself…</h2>"));
        assert!(s.contains("<h2>Open epics worth knowing about</h2>"));
        assert!(s.contains("node bin/driver-demo.mjs"));
        assert!(s.contains("system.docs.getting_started '{}' &gt; docs/getting-started.html"));
    }
    #[test]
    fn ignores_input_and_context() {
        let db = Connection::open_in_memory().unwrap();
        let a = gettingStartedHtml(&json!({}), &db).unwrap();
        let b = gettingStartedHtml(&json!({"whatever": 1, "nested": {"x": [1,2,3]}}), &db).unwrap();
        assert_eq!(a, b);
    }
    #[test]
    fn deterministic() {
        assert_eq!(out(), out());
    }
}
