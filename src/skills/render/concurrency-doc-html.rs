//! concurrency-doc-html.rs — twin of skills/render/concurrency-doc-html.mjs (standard lane; pure docs, no db).
//! NAMES ARE CONTRACT: `concurrencyDocHtml` (default). Renders the concurrency-model reference page.
// Template literals are CRLF in source but JS normalizes line terminators to LF, so the output uses
// "\n": the twin joins line pieces with explicit "\n" and never embeds a physical newline.
use rusqlite::Connection;
use serde_json::Value;

// CSS constant — byte-exact value of the JS `const CSS` template (leading + trailing "\n").
const CSS: &str = "\n:root { --bg:#0e1116; --panel:#161b22; --border:#30363d; --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950; --warn:#d29922; --bad:#f85149; }\nbody { font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; line-height:1.55; }\n.wrap { max-width:920px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.9em; margin:0 0 0.2em; }\nh2 { font-size:1.25em; font-weight:600; margin-top:2em; padding-bottom:0.3em; border-bottom:1px solid var(--border); }\nh3 { font-size:1em; margin-top:1.4em; }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; }\n.lede { color:var(--muted); }\ncode, pre { font-family:ui-monospace,monospace; font-size:0.86em; }\ncode { background:#0d1117; padding:1px 5px; border-radius:3px; border:1px solid var(--border); }\npre { background:#0d1117; padding:0.9em 1.1em; border-radius:6px; border:1px solid var(--border); overflow-x:auto; }\n.panel { background:var(--panel); border:1px solid var(--border); border-radius:6px; padding:0.9em 1.1em; margin:0.8em 0; }\n.panel.good { border-color:var(--good); }\n.panel.warn { border-color:var(--warn); }\n.panel.bad  { border-color:var(--bad); }\nul { padding-left:1.4em; }\nli { margin:0.3em 0; }\na { color:var(--accent); }\n";

// NAV constant — byte-exact value of the JS `const NAV` template (single line, verbatim).
const NAV: &str = r#"<nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,&quot;Segoe UI&quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>"#;

pub fn concurrencyDocHtml(_input: &Value, _db: &Connection) -> Result<Value, String> {
    let lines: Vec<String> = vec![
        r#"<!DOCTYPE html>"#.to_string(),
        [r#"<html><head><meta charset="UTF-8"><title>antiloki · concurrency</title><style>"#, CSS, r#"</style></head>"#].concat(),
        [r#"<body>"#, NAV, r#"<div class="wrap">"#].concat(),
        r#"<div class="kicker">antiloki-v3 · operations</div>"#.to_string(),
        r#"<h1>Concurrency model</h1>"#.to_string(),
        r#"<p class="lede">What happens when two antiloki processes run against the same DBs? Hard answers, with the escape hatches documented.</p>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>WAL mode is on</h2>"#.to_string(),
        r#"<p>As of v0.9, <code>open-db.mjs</code> sets <code>PRAGMA journal_mode = WAL</code> + <code>PRAGMA synchronous = NORMAL</code> on every connect, for every attached DB in the set. Effects:</p>"#.to_string(),
        r#"<ul>"#.to_string(),
        r#"  <li>Multiple readers + ONE writer can coexist concurrently (vs. journal mode's reader-OR-writer)</li>"#.to_string(),
        r#"  <li>Writers don't block readers and vice versa for the common case</li>"#.to_string(),
        r#"  <li>Crashes survive better — WAL is replayed on next open</li>"#.to_string(),
        r#"  <li>Sidecar files appear: <code>state.sqlite-wal</code>, <code>state.sqlite-shm</code> (gitignored already)</li>"#.to_string(),
        r#"</ul>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>What's safe to run concurrently</h2>"#.to_string(),
        r#"<div class="panel good">"#.to_string(),
        r#"  <h3>✓ Concurrent reads</h3>"#.to_string(),
        r#"  Multiple <code>system.consistency.scan</code>, <code>board-keeper.card.list</code>, <code>system.docs.*</code> in parallel — fine. WAL was designed for this."#.to_string(),
        r#"</div>"#.to_string(),
        r#"<div class="panel good">"#.to_string(),
        r#"  <h3>✓ Read-only TUI + a writing CLI</h3>"#.to_string(),
        r#"  <code>npm run tui</code> opens with <code>readOnly:true</code>. You can write from another terminal while watching."#.to_string(),
        r#"</div>"#.to_string(),
        r#"<div class="panel warn">"#.to_string(),
        r#"  <h3>⚠ Multiple writers on the SAME DB</h3>"#.to_string(),
        r#"  Two simultaneous <code>card.add</code> calls: SQLite serializes them via the WAL writer lock. Throughput is fine for human cadence. Under high contention (auto-loops, tests in parallel), you'll see <code>SQLITE_BUSY</code> errors."#.to_string(),
        r#"</div>"#.to_string(),
        r#"<div class="panel bad">"#.to_string(),
        r#"  <h3>✗ Two driver.run loops simultaneously</h3>"#.to_string(),
        r#"  Not safe today. Both loops will pick the same actionable card. The scheduler has no inter-process lock. <code>brain.brain_locks</code> exists but isn't used at the driver tick boundary."#.to_string(),
        r#"  <br><br>"#.to_string(),
        r#"  <strong>Rule:</strong> only one <code>system.driver.run</code> at a time. Enforce via <code>.antiloki/driver-halt</code> file + manual coordination, or wait for a future card that wires <code>brain_locks</code> into the driver-tick entry path."#.to_string(),
        r#"</div>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>Backup + restore</h2>"#.to_string(),
        r#"<pre><code># Atomic snapshot of all 5 DBs (uses VACUUM INTO — safe on live WAL)"#.to_string(),
        r#"npm run backup"#.to_string(),
        r#"# →  .antiloki/backups/2026-05-19T17-12-34-456Z/"#.to_string(),
        r#""#.to_string(),
        r#"# Restore (refuses to overwrite without --force)"#.to_string(),
        r#"npm run restore .antiloki/backups/2026-05-19T17-12-34-456Z/ -- --force</code></pre>"#.to_string(),
        r#"<p class="lede">VACUUM INTO produces a single-file copy regardless of WAL state. Safer than fs.copyFile on a live DB.</p>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>Tests run serially</h2>"#.to_string(),
        r#"<p>The test suite uses <code>--test-concurrency=1</code> on purpose. Each test creates its own <code>:memory:</code> DB or uses isolated temp paths; running them in parallel would cause file-handle contention on the shared project DB.</p>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>Tests are NOT a substitute for the concurrency story</h2>"#.to_string(),
        r#"<p>None of the 961 tests exercises real multi-process contention against the same on-disk DB. The model above is what you get; if you hit edge cases, file a card.</p>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>If you must run multiple processes</h2>"#.to_string(),
        r#"<ul>"#.to_string(),
        r#"  <li><strong>Different DBs per process</strong> — use <code>ANTILOKI_DB</code>, <code>ANTILOKI_MEMORY_DB</code>, etc. env vars to point each process at its own DB set. Then merge later via export/import skills (not yet built).</li>"#.to_string(),
        r#"  <li><strong>Read-only secondary processes</strong> — pass <code>{readOnly:true}</code> to <code>openDb</code>. TUI does this.</li>"#.to_string(),
        r#"  <li><strong>Wait for proper distributed coordination</strong> — out of scope for the single-developer model.</li>"#.to_string(),
        r#"</ul>"#.to_string(),
        r#""#.to_string(),
        r#"<h2>Recovery if things go wrong</h2>"#.to_string(),
        r#"<pre><code># Find your most recent backup"#.to_string(),
        r#"ls -la .antiloki/backups/ | tail -5"#.to_string(),
        r#""#.to_string(),
        r#"# Restore that snapshot (overwrites live DBs)"#.to_string(),
        r#"npm run restore .antiloki/backups/&lt;ts&gt;/ -- --force"#.to_string(),
        r#""#.to_string(),
        r#"# Or just delete the broken DB and let open-db reseed (loses data!)"#.to_string(),
        r#"rm .antiloki/db/state.sqlite     # ← only if you're sure"#.to_string(),
        r#"node bin/antiloki.mjs system.action.find '{"query":""}'   # triggers reopen + reseed</code></pre>"#.to_string(),
        r#""#.to_string(),
        r#"<p class="lede">Generated by <code>system.docs.concurrency</code>. Filed under FEAT-PROD-READINESS-WAL-BACKUP.</p>"#.to_string(),
        r#"</div></body></html>"#.to_string(),
    ];
    Ok(Value::String(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn out() -> String {
        match concurrencyDocHtml(&json!({}), &Connection::open_in_memory().unwrap()).unwrap() {
            Value::String(s) => s,
            _ => unreachable!(),
        }
    }
    #[test]
    fn byte_exact_structure() {
        let s = out();
        assert!(s.starts_with("<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>antiloki · concurrency</title><style>\n:root {"));
        assert!(s.ends_with("</div></body></html>"));
        assert!(!s.contains('\r'), "output must be LF-only");
        assert!(s.contains("← antiloki docs</a></nav>"));
        assert!(s.contains("<h2>WAL mode is on</h2>"));
        assert!(s.contains("<h2>What's safe to run concurrently</h2>"));
        assert!(s.contains("<h2>Backup + restore</h2>"));
        assert!(s.contains("<h2>Tests run serially</h2>"));
        assert!(s.contains("<h2>Tests are NOT a substitute for the concurrency story</h2>"));
        assert!(s.contains("<h2>If you must run multiple processes</h2>"));
        assert!(s.contains("<h2>Recovery if things go wrong</h2>"));
        assert!(s.contains("system.action.find '{\"query\":\"\"}'   # triggers reopen + reseed</code></pre>"));
        assert!(s.contains("Filed under FEAT-PROD-READINESS-WAL-BACKUP.</p>"));
    }
    #[test]
    fn deterministic() {
        assert_eq!(out(), out());
    }
    #[test]
    fn ignores_input() {
        let a = concurrencyDocHtml(&json!({}), &Connection::open_in_memory().unwrap()).unwrap();
        let b = concurrencyDocHtml(&json!({"foo": "bar"}), &Connection::open_in_memory().unwrap()).unwrap();
        assert_eq!(a, b);
    }
}
