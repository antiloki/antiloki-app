//! sandbox-doc-html.rs — twin of skills/render/sandbox-doc-html.mjs (standard lane; pure docs, no db).
//! NAMES ARE CONTRACT: `sandboxDocHtml` (default). Renders the copy-on-write sandbox reference page.
// Template literals are CRLF in source but JS normalizes line terminators to LF, so the output uses
// "\n": the twin joins line pieces with explicit "\n" and never embeds a physical newline. The
// generatedAt timestamp is frozen (conform clock) to 2026-01-01T00:00:00.000Z; escHtml leaves it
// unchanged but is applied for fidelity.
use rusqlite::Connection;
use serde_json::Value;

// CSS constant — byte-exact value of the JS `const CSS` template (leading + trailing "\n").
const CSS: &str = "\n:root { --bg:#0e1116; --panel:#161b22; --panel2:#1f2530; --border:#30363d;\n        --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950;\n        --warn:#d29922; --bad:#f85149; --code-bg:#0d1117; }\n* { box-sizing:border-box; }\nbody { font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }\n.wrap { max-width:1080px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.9em; font-weight:700; letter-spacing:-0.02em; margin:0 0 0.2em; }\nh2 { font-size:1.3em; font-weight:600; margin-top:2.2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); }\nh3 { font-size:1.05em; font-weight:600; margin-top:1.5em; }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; margin-bottom:0.2em; }\n.lede { color:var(--muted); font-size:1.02em; max-width:680px; }\n.meta { color:var(--muted); font-size:0.82em; margin:0.3em 0 1.5em; font-family:ui-monospace,monospace; }\ncode, pre { font-family:ui-monospace,\"JetBrains Mono\",Consolas,monospace; font-size:0.86em; }\ncode { background:var(--code-bg); padding:1px 5px; border-radius:3px; border:1px solid var(--border); }\npre { background:var(--code-bg); padding:0.9em 1.1em; border-radius:6px; border:1px solid var(--border); overflow-x:auto; }\ntable { width:100%; border-collapse:collapse; margin:0.6em 0 1.5em; font-size:0.9em; }\nth, td { padding:0.55em 0.8em; text-align:left; border-bottom:1px solid var(--border); vertical-align:top; }\nth { color:var(--muted); font-weight:500; font-size:0.82em; background:var(--panel); }\n.panel.warn { background:var(--panel); border:1px solid var(--warn); border-radius:6px; padding:0.9em 1.1em; margin:0.8em 0; }\n.panel.good { background:var(--panel); border:1px solid var(--good); border-radius:6px; padding:0.9em 1.1em; margin:0.8em 0; }\n.panel.bad  { background:var(--panel); border:1px solid var(--bad);  border-radius:6px; padding:0.9em 1.1em; margin:0.8em 0; }\nul { padding-left:1.4em; }\nli { margin:0.3em 0; }\n.muted { color:var(--muted); }\n";

// NAV constant — byte-exact value of the JS `const NAV` template (single line, verbatim).
const NAV: &str = r#"<nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,&quot;Segoe UI&quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>"#;

// JS escHtml: String(s ?? '') then & < > " replaced in that order.
fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub fn sandboxDocHtml(_input: &Value, _db: &Connection) -> Result<Value, String> {
    let generated_at = crate::clock::now_iso_nodb();
    let gen_esc = esc_html(&generated_at);
    let lines: Vec<String> = vec![
        r#"<!DOCTYPE html>"#.to_string(),
        [r#"<html><head><meta charset="UTF-8"><title>antiloki · sandbox</title><style>"#, CSS, r#"</style></head>"#].concat(),
        [r#"<body>"#, NAV, r#"<div class="wrap">"#].concat(),
        r#"<div class="kicker">antiloki-v3 · sandbox</div>"#.to_string(),
        r#"<h1>Copy-on-write sandbox — DEV-on-copies, QA promotes atomically</h1>"#.to_string(),
        r#"<p class="lede">Every contract gets a per-slug sandbox folder. DEV writes go to the sandbox. Originals only change when QA approves and the substrate atomically replaces them. Tamper-evident: if anything else changed the originals during DEV/QA, the promotion aborts.</p>"#.to_string(),
        [r#"<p class="meta">Generated <code>"#, gen_esc.as_str(), r#"</code> · regen <code>node bin/antiloki.mjs system.docs.sandbox '{}' &gt; docs/sandbox.html</code></p>"#].concat(),
        r#""#.to_string(),
        r#"<h2>Lifecycle</h2>"#.to_string(),
        r#"<ol>"#.to_string(),
        r#"  <li><strong>TL approves contract</strong> → status='tl_approved'.</li>"#.to_string(),
        r#"  <li><strong>Driver fires <code>tl.sandbox.create</code></strong> → copies every file in <code>contract.files_touched</code> to <code>.antiloki/sandbox/&lt;slug&gt;/&lt;relpath&gt;</code>. Captures sha256 of each ORIGINAL into <code>contract.original_shas_json</code>. Sets <code>contract.sandbox_path</code>.</li>"#.to_string(),
        r#"  <li><strong>DEV implements via <code>dev.implement.code</code></strong> → AI produces file changes. Every write routes through <code>dev.file.write({path, content, contract_slug})</code>, which goes through <code>sandbox-resolve-path</code> and lands in the sandbox.</li>"#.to_string(),
        r#"  <li><strong>QA gates via <code>qa.gate.contract.verify</code></strong> → runs scope-check + acceptance against sandbox files. Persists qa_runs row.</li>"#.to_string(),
        r#"  <li><strong>On pass</strong>: <code>qa.sandbox.promote</code> re-hashes originals, compares to captured shas, atomically renames sandbox files → originals. Wipes sandbox. Sets <code>contract.sandbox_path = NULL</code>. Status → 'done'.</li>"#.to_string(),
        r#"  <li><strong>On fail</strong>: sandbox preserved for retry. Status stays at 'qa' / 'impl'. Driver may re-fire DEV. Explicit <code>qa.sandbox.reset</code> wipes if needed.</li>"#.to_string(),
        r#"</ol>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>Safety guarantees</h2>"#.to_string(),
        r#"<div class="panel good">"#.to_string(),
        r#"  <h3>Originals untouched until QA passes</h3>"#.to_string(),
        r#"  <p>DEV cannot modify a project file directly while a contract is active. <code>dev.file.write</code> refuses when <code>contract.status ∈ {impl, qa, done}</code> AND <code>sandbox_path</code> is NULL. After TL creates the sandbox, all writes route to it.</p>"#.to_string(),
        r#"</div>"#.to_string(),
        r#"<div class="panel good">"#.to_string(),
        r#"  <h3>Scope enforcement at the file-write boundary</h3>"#.to_string(),
        r#"  <p><code>sandbox-resolve-path</code> rejects any <code>relpath</code> not in <code>contract.files_touched</code>. DEV cannot route around the contract by hand-rolling a path — every sandbox-aware skill goes through this chokepoint.</p>"#.to_string(),
        r#"</div>"#.to_string(),
        r#"<div class="panel warn">"#.to_string(),
        r#"  <h3>Tamper detection at promote-time</h3>"#.to_string(),
        r#"  <p><code>qa-sandbox-promote</code> re-hashes every original and compares against <code>contract.original_shas_json</code> (captured at tl-sandbox-create time). If ANY sha diverged — or a file was created/deleted externally — promote ABORTS with TAMPERED. Sandbox is preserved for human review. <em>The originals are NEVER overwritten with someone else's work.</em></p>"#.to_string(),
        r#"</div>"#.to_string(),
        r#"<div class="panel good">"#.to_string(),
        r#"  <h3>Atomic stage-then-rename</h3>"#.to_string(),
        r#"  <p>Every sandbox file is first copied to <code>&lt;original&gt;.antiloki-staging</code>. Only AFTER every staging copy succeeds do we batch-rename them to final paths. <code>renameSync</code> is atomic on most OSes; staging-first keeps the failure window tight.</p>"#.to_string(),
        r#"</div>"#.to_string(),
        r#"<div class="panel bad">"#.to_string(),
        r#"  <h3>HARD_BLACKLIST applies even via a contract</h3>"#.to_string(),
        r#"  <p>Writes to <code>.git/</code>, <code>node_modules/</code>, <code>.antiloki/auth/</code>, <code>state.sqlite</code>, etc. are blocked regardless of what <code>contract.files_touched</code> contains. Future contract authoring lints should reject these too, but the enforcement is in file-write so even a buggy contract can't break the system.</p>"#.to_string(),
        r#"</div>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>The actions</h2>"#.to_string(),
        r#"<table>"#.to_string(),
        r#"  <thead><tr><th>action</th><th>owner</th><th>what it does</th></tr></thead>"#.to_string(),
        r#"  <tbody>"#.to_string(),
        r#"    <tr><td><code>tl.sandbox.create</code></td>  <td>tl</td>  <td>Copies files_touched into <code>.antiloki/sandbox/&lt;slug&gt;</code>, captures original shas</td></tr>"#.to_string(),
        r#"    <tr><td><code>dev.file.write</code></td>    <td>dev</td> <td>Sandbox-aware via <code>contract_slug</code> arg; routes through resolve-path</td></tr>"#.to_string(),
        r#"    <tr><td><code>dev.file.read</code></td>     <td>dev</td> <td>Sandbox-aware; reads from sandbox if present, falls back to original</td></tr>"#.to_string(),
        r#"    <tr><td><code>dev.implement.code</code></td><td>dev</td> <td>AI-driven code generation; writes go through sandbox-aware <code>fileWrite</code></td></tr>"#.to_string(),
        r#"    <tr><td><code>qa.gate.contract.verify</code></td><td>qa</td><td>Composes scope-check + acceptance; on pass → <code>qa.sandbox.promote</code> + status='done'</td></tr>"#.to_string(),
        r#"    <tr><td><code>qa.sandbox.promote</code></td><td>qa</td>  <td>Tamper-checks + atomically replaces originals; wipes sandbox</td></tr>"#.to_string(),
        r#"    <tr><td><code>qa.sandbox.reset</code></td>  <td>qa</td>  <td>Wipes sandbox for a contract; clears <code>sandbox_path</code>; optional correction memory</td></tr>"#.to_string(),
        r#"  </tbody>"#.to_string(),
        r#"</table>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>State-machine integration (driver)</h2>"#.to_string(),
        r#"<p>The driver's state machine has a rule <code>contract-tl-approved-no-sandbox</code> that fires BEFORE the DEV-start rule. So the lifecycle is automatic:</p>"#.to_string(),
        r#"<pre><code>tl_approved + no sandbox  → driver dispatches tl.sandbox.create"#.to_string(),
        r#"tl_approved + sandbox     → driver dispatches dev.contract.load"#.to_string(),
        r#"impl + no qa              → driver dispatches dev.implement.code (writes to sandbox)"#.to_string(),
        r#"impl + qa_failed + N&lt;3   → driver dispatches dev.implement.code (retry, sandbox preserved)"#.to_string(),
        r#"impl + qa_failed + N≥3   → driver escalates (sandbox preserved for review)"#.to_string(),
        r#"qa                       → driver dispatches qa.gate.contract.verify"#.to_string(),
        r#"qa pass                  → qa.sandbox.promote auto-fires inside the gate</code></pre>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>Inspecting an in-flight sandbox</h2>"#.to_string(),
        r#"<pre><code># List all active sandboxes"#.to_string(),
        r#"ls .antiloki/sandbox/"#.to_string(),
        r#""#.to_string(),
        r#"# Diff a sandbox against originals"#.to_string(),
        r#"diff -r &lt;original&gt; .antiloki/sandbox/&lt;slug&gt;/"#.to_string(),
        r#""#.to_string(),
        r#"# Resync the catalog after sandbox changes"#.to_string(),
        r#"node bin/antiloki.mjs system.catalog.refresh '{}'</code></pre>"#.to_string(),
        r#""#.to_string(),
        r#"<p class="muted">Concept proof: <code>node bin/sandbox-demo.mjs</code> runs every code path on a synthetic contract in &lt;2s.</p>"#.to_string(),
        r#"</div></body></html>"#.to_string(),
    ];
    Ok(Value::String(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn out() -> String {
        match sandboxDocHtml(&json!({}), &Connection::open_in_memory().unwrap()).unwrap() {
            Value::String(s) => s,
            _ => unreachable!(),
        }
    }
    #[test]
    fn byte_exact_structure() {
        let s = out();
        assert!(s.starts_with("<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>antiloki · sandbox</title><style>\n:root {"));
        assert!(s.ends_with("</div></body></html>"));
        assert!(!s.contains('\r'), "output must be LF-only");
        assert!(s.contains("<p class=\"meta\">Generated <code>2026-01-01T00:00:00.000Z</code> · regen"));
        assert!(s.contains("← antiloki docs</a></nav>"));
        assert!(s.contains("<h2>Lifecycle</h2>"));
        assert!(s.contains("<h2>Safety guarantees</h2>"));
        assert!(s.contains("<h2>The actions</h2>"));
        assert!(s.contains("<code>contract.status ∈ {impl, qa, done}</code>"));
        assert!(s.contains("system.docs.sandbox '{}' &gt; docs/sandbox.html"));
    }
    #[test]
    fn deterministic() {
        assert_eq!(out(), out());
    }
}
