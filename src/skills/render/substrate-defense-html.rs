//! render/substrate-defense-html.rs — twin of skills/render/substrate-defense-html.mjs (db READ
//! lane, cross-db: laws.laws, memory.memories). NAMES ARE CONTRACT: `substrateDefenseHtml`
//! (default). Self-contained HTML doc of the substrate's defense-in-depth model, built from live
//! counts (laws/actions/agents/sacred_timeline/correction memories). CSS + all three inline SVG
//! diagrams and the fully-static prose sections are transcribed byte-for-byte from the JS template
//! literals, including the pre-existing `<body>quot;` typo in the nav style attribute — this is a
//! verbatim transcription, not a fix (byte-identical output is the contract). The JS query against
//! memory.memories filters on a column named `memory_type`, but the real schema's column is `type`
//! — that query always throws and is caught (corrections stays `[]`); the twin reproduces the SAME
//! query verbatim, so it fails/catches identically in both lanes.
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS `${v}`
// JS String(v) — used inside escHtml(v), which does String(v) before replacing.
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
// escHtml(s) = String(s).replace(&).replace(<).replace(>).replace(")  — & first, so produced
// entities aren't re-escaped.
fn esc_str(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn esc_html(v: &Value) -> String { esc_str(&js_string(v)) }

fn first_count(v: &Value) -> i64 {
    v.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(|c| c.as_i64()).unwrap_or(0)
}

const CSS: &str = "\n:root {\n  --bg:#0e1116; --panel:#161b22; --panel2:#1f2530; --border:#30363d;\n  --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950;\n  --warn:#d29922; --bad:#f85149; --code-bg:#0d1117;\n}\n* { box-sizing: border-box; }\nbody { font-family: -apple-system, BlinkMacSystemFont, \"Segoe UI\", system-ui, sans-serif; background: var(--bg); color: var(--fg); margin: 0; padding: 0; line-height: 1.55; }\n.wrap { max-width: 1080px; margin: 0 auto; padding: 2em 1.5em 4em; }\nh1 { font-size: 2em; font-weight: 700; letter-spacing: -0.02em; margin: 0 0 0.2em; }\nh2 { font-size: 1.45em; font-weight: 600; margin-top: 2.5em; padding-bottom: 0.3em; border-bottom: 1px solid var(--border); }\nh3 { font-size: 1.1em; font-weight: 600; margin-top: 1.8em; color: var(--fg); }\np { color: var(--fg); }\n.lede { color: var(--muted); font-size: 1.05em; max-width: 680px; }\n.meta { color: var(--muted); font-size: 0.85em; margin: 0.3em 0 1.5em; font-family: ui-monospace, monospace; }\ncode, pre { font-family: ui-monospace, \"JetBrains Mono\", Consolas, monospace; font-size: 0.88em; }\ncode { background: var(--code-bg); padding: 1px 5px; border-radius: 3px; border: 1px solid var(--border); }\npre { background: var(--code-bg); padding: 0.9em 1.1em; border-radius: 6px; border: 1px solid var(--border); overflow-x: auto; }\npre code { background: none; border: none; padding: 0; }\ntable { width: 100%; border-collapse: collapse; margin: 0.6em 0 1.4em; font-size: 0.92em; }\nth, td { padding: 0.65em 0.9em; text-align: left; border-bottom: 1px solid var(--border); vertical-align: top; }\nth { color: var(--muted); font-weight: 500; font-size: 0.85em; background: var(--panel); }\ntr:hover td { background: var(--panel2); }\n.panel { background: var(--panel); border: 1px solid var(--border); border-radius: 8px; padding: 1.1em 1.3em; margin: 1em 0; }\n.panel.danger { border-color: var(--bad); }\n.panel.warn   { border-color: var(--warn); }\n.panel.good   { border-color: var(--good); }\n.tag { display: inline-block; padding: 1px 7px; border-radius: 10px; font-size: 0.78em; font-family: ui-monospace, monospace; margin-right: 4px; }\n.tag.hard  { background: rgba(248,81,73,0.15);  color: var(--bad);  border: 1px solid rgba(248,81,73,0.4); }\n.tag.soft  { background: rgba(210,153,34,0.15); color: var(--warn); border: 1px solid rgba(210,153,34,0.4); }\n.tag.good  { background: rgba(63,185,80,0.15);  color: var(--good); border: 1px solid rgba(63,185,80,0.4); }\n.muted { color: var(--muted); }\nsvg.diag { display: block; margin: 1em auto; max-width: 100%; height: auto; background: var(--panel); border: 1px solid var(--border); border-radius: 8px; }\n.legend { font-size: 0.82em; color: var(--muted); text-align: center; margin-top: -0.6em; margin-bottom: 1.5em; }\n.kicker { color: var(--accent); font-family: ui-monospace, monospace; font-size: 0.78em; text-transform: uppercase; letter-spacing: 0.08em; margin-bottom: 0.2em; }\n.proof { background: rgba(63,185,80,0.06); border-left: 3px solid var(--good); padding: 0.6em 0.9em; margin: 0.6em 0; border-radius: 0 4px 4px 0; font-size: 0.92em; }\n.proof::before { content: \"PROOF · \"; color: var(--good); font-family: ui-monospace, monospace; font-size: 0.78em; font-weight: 600; }\n.lie { background: rgba(248,81,73,0.06); border-left: 3px solid var(--bad); padding: 0.6em 0.9em; margin: 0.6em 0; border-radius: 0 4px 4px 0; font-size: 0.92em; }\n.lie::before { content: \"LIE PATTERN · \"; color: var(--bad); font-family: ui-monospace, monospace; font-size: 0.78em; font-weight: 600; }\nul { padding-left: 1.4em; }\nli { margin: 0.3em 0; }\nhr { border: 0; border-top: 1px solid var(--border); margin: 2.5em 0; }\n";

// headerSection — no interpolation, so it's identical for every db state.
const HEADER_SECTION: &str = "\n    <div class=\"kicker\">antiloki-v3 · defense-in-depth documentation</div>\n    <h1>How the substrate prevents AI from lying without consequences</h1>\n    <p class=\"lede\">This page documents the five enforcement layers that surround every AI action in this repo. Every claim below maps to a command you can run to verify it for yourself.</p>\n    <div class=\"meta\">Auto-generated from <code>.antiloki/db/laws.sqlite</code>. To re-render: <code>node bin/antiloki.mjs system.docs.defense '{}'</code></div>\n  ";

// substrateSection — no interpolation beyond svgFiveDbs(), which itself maps over a hardcoded
// array (no db/input dependency) — always the same string. Baked in verbatim.
const SUBSTRATE_SECTION: &str = "\n    <h2>2. The 5-DB substrate</h2>\n    <p>Five SQLite databases, attached as schemas onto a single connection. They are physically separable but logically queryable in one breath. Each one answers exactly one question about the system:</p>\n    \n<svg class=\"diag\" viewBox=\"0 0 860 180\" xmlns=\"http://www.w3.org/2000/svg\">\n  <rect x=\"20\" y=\"6\" width=\"820\" height=\"28\" rx=\"4\" fill=\"#1f2530\" stroke=\"#58a6ff\" stroke-width=\"1.2\"/>\n  <text x=\"430\" y=\"24\" text-anchor=\"middle\" fill=\"#58a6ff\" font-family=\"ui-monospace, monospace\" font-size=\"12\" font-weight=\"600\">ATTACHed via openDb() — one connection, five logical schemas</text>\n  <line x1=\"110\" y1=\"40\" x2=\"110\" y2=\"20\" stroke=\"#30363d\" stroke-width=\"1.5\"/><line x1=\"270\" y1=\"40\" x2=\"270\" y2=\"20\" stroke=\"#30363d\" stroke-width=\"1.5\"/><line x1=\"430\" y1=\"40\" x2=\"430\" y2=\"20\" stroke=\"#30363d\" stroke-width=\"1.5\"/><line x1=\"590\" y1=\"40\" x2=\"590\" y2=\"20\" stroke=\"#30363d\" stroke-width=\"1.5\"/><line x1=\"750\" y1=\"40\" x2=\"750\" y2=\"20\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n  \n    <rect x=\"40\" y=\"40\" width=\"140\" height=\"80\" rx=\"6\" fill=\"#58a6ff\" fill-opacity=\"0.15\" stroke=\"#58a6ff\" stroke-width=\"1.5\"/>\n    <text x=\"110\" y=\"72\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">state.sqlite</text>\n    <text x=\"110\" y=\"92\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">what IS</text>\n  \n    <rect x=\"200\" y=\"40\" width=\"140\" height=\"80\" rx=\"6\" fill=\"#bc8cff\" fill-opacity=\"0.15\" stroke=\"#bc8cff\" stroke-width=\"1.5\"/>\n    <text x=\"270\" y=\"72\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">memory.sqlite</text>\n    <text x=\"270\" y=\"92\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">what EXPERIENCED</text>\n  \n    <rect x=\"360\" y=\"40\" width=\"140\" height=\"80\" rx=\"6\" fill=\"#ff7b72\" fill-opacity=\"0.15\" stroke=\"#ff7b72\" stroke-width=\"1.5\"/>\n    <text x=\"430\" y=\"72\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">brain.sqlite</text>\n    <text x=\"430\" y=\"92\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">what THINKS</text>\n  \n    <rect x=\"520\" y=\"40\" width=\"140\" height=\"80\" rx=\"6\" fill=\"#3fb950\" fill-opacity=\"0.15\" stroke=\"#3fb950\" stroke-width=\"1.5\"/>\n    <text x=\"590\" y=\"72\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">laws.sqlite</text>\n    <text x=\"590\" y=\"92\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">what MUST/MUST NOT</text>\n  \n    <rect x=\"680\" y=\"40\" width=\"140\" height=\"80\" rx=\"6\" fill=\"#d29922\" fill-opacity=\"0.15\" stroke=\"#d29922\" stroke-width=\"1.5\"/>\n    <text x=\"750\" y=\"72\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">knowledge.sqlite</text>\n    <text x=\"750\" y=\"92\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">what LEARNED</text>\n  \n  <text x=\"430\" y=\"160\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">Triggers on sacred_timeline (state) enforce INSERT-only + digital_print chain.</text>\n</svg>\n    <table>\n      <tr><th>database</th><th>answers</th><th>writable how</th></tr>\n      <tr><td><code>state.sqlite</code></td><td>What does the system look like right now?</td><td>via with-audit only (DB trigger enforces sacred_timeline INSERT-only)</td></tr>\n      <tr><td><code>memory.sqlite</code></td><td>What has the system experienced + learned?</td><td>via <code>system.memory.add</code> (correction memories are DPO pairs)</td></tr>\n      <tr><td><code>brain.sqlite</code></td><td>What is the system thinking / benching / deciding?</td><td>via <code>system.driver.*</code> + <code>system.bench.*</code></td></tr>\n      <tr><td><code>laws.sqlite</code></td><td>What must / must not the system do?</td><td>via <code>system.law.add</code> + <code>system.law.supersede</code> (append-only)</td></tr>\n      <tr><td><code>knowledge.sqlite</code></td><td>What has been learned from external sources?</td><td>vault-curated extracts, write-only</td></tr>\n    </table>\n    <div class=\"proof\">Verify the 5 schemas: <code>node bin/antiloki.mjs --unsafe-skill anatomy-snapshot '{}' | head -50</code></div>\n  ";

// layersSection — same reasoning as substrateSection (svgFiveLayers() is data-independent).
const LAYERS_SECTION: &str = "\n    <h2>3. The 5 enforcement layers</h2>\n    <p>An attempt to violate a rule must pass through all five layers undetected. The layers are heterogeneous on purpose — a bypass technique that defeats one layer typically can't defeat the others.</p>\n    \n<svg class=\"diag\" viewBox=\"0 0 860 340\" xmlns=\"http://www.w3.org/2000/svg\">\n  <text x=\"430\" y=\"20\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"13\" font-weight=\"600\">An AI attempt to \"lie\" must pass through ALL five layers. Any one stops it.</text>\n  \n    <rect x=\"40\" y=\"30\" width=\"780\" height=\"46\" rx=\"4\" fill=\"#d29922\" fill-opacity=\"0.1\" stroke=\"#d29922\" stroke-width=\"1.3\"/>\n    <text x=\"60\" y=\"58\" fill=\"#d29922\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">1. Document layer</text>\n    <text x=\"60\" y=\"74\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">AGENTS.md + CLAUDE.md → AI reads laws before acting</text>\n  \n    <rect x=\"40\" y=\"90\" width=\"780\" height=\"46\" rx=\"4\" fill=\"#bc8cff\" fill-opacity=\"0.1\" stroke=\"#bc8cff\" stroke-width=\"1.3\"/>\n    <text x=\"60\" y=\"118\" fill=\"#bc8cff\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">2. RAG-surface layer</text>\n    <text x=\"60\" y=\"134\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">law-surface-for-context injects relevant laws into action execution</text>\n  \n    <rect x=\"40\" y=\"150\" width=\"780\" height=\"46\" rx=\"4\" fill=\"#58a6ff\" fill-opacity=\"0.1\" stroke=\"#58a6ff\" stroke-width=\"1.3\"/>\n    <text x=\"60\" y=\"178\" fill=\"#58a6ff\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">3. Action-run hook layer</text>\n    <text x=\"60\" y=\"194\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">hard-law hooks fire at dispatch (fail-closed) → PERMISSION_DENIED</text>\n  \n    <rect x=\"40\" y=\"210\" width=\"780\" height=\"46\" rx=\"4\" fill=\"#3fb950\" fill-opacity=\"0.1\" stroke=\"#3fb950\" stroke-width=\"1.3\"/>\n    <text x=\"60\" y=\"238\" fill=\"#3fb950\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">4. DB-trigger layer</text>\n    <text x=\"60\" y=\"254\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">SQLite triggers on sacred_timeline reject UPDATE/DELETE/forged sha</text>\n  \n    <rect x=\"40\" y=\"270\" width=\"780\" height=\"46\" rx=\"4\" fill=\"#ff7b72\" fill-opacity=\"0.1\" stroke=\"#ff7b72\" stroke-width=\"1.3\"/>\n    <text x=\"60\" y=\"298\" fill=\"#ff7b72\" font-family=\"ui-monospace, monospace\" font-size=\"14\" font-weight=\"600\">5. Consistency-scan layer</text>\n    <text x=\"60\" y=\"314\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">Pre-commit hook runs 16-dimension scan → blocks drift commits</text>\n  \n  \n    <line x1=\"430\" y1=\"76\" x2=\"430\" y2=\"90\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n    <polygon points=\"426,88 434,88 430,94\" fill=\"#30363d\"/>\n  \n    <line x1=\"430\" y1=\"136\" x2=\"430\" y2=\"150\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n    <polygon points=\"426,148 434,148 430,154\" fill=\"#30363d\"/>\n  \n    <line x1=\"430\" y1=\"196\" x2=\"430\" y2=\"210\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n    <polygon points=\"426,208 434,208 430,214\" fill=\"#30363d\"/>\n  \n    <line x1=\"430\" y1=\"256\" x2=\"430\" y2=\"270\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n    <polygon points=\"426,268 434,268 430,274\" fill=\"#30363d\"/>\n  \n  <text x=\"430\" y=\"328\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">Top = soft (advisory). Bottom = hard (mechanically enforced).</text>\n</svg>\n    <h3>Per-layer mechanics</h3>\n    <table>\n      <tr><th>layer</th><th>where</th><th>what it catches</th><th>verify</th></tr>\n      <tr>\n        <td><strong>1. Document</strong></td>\n        <td><code>AGENTS.md</code> + <code>CLAUDE.md</code></td>\n        <td>\"AI hasn't read the rules\" — it now does, every session start</td>\n        <td><code>ls -la AGENTS.md CLAUDE.md</code></td>\n      </tr>\n      <tr>\n        <td><strong>2. RAG-surface</strong></td>\n        <td><code>law-surface-for-context</code></td>\n        <td>\"AI forgot the rules mid-session\" — top-5 relevant soft laws injected per dispatch</td>\n        <td><code>node bin/antiloki.mjs --unsafe-skill law-surface-for-context '{\"action\":\"board-keeper.card.add\",\"agent\":\"dev\"}'</code></td>\n      </tr>\n      <tr>\n        <td><strong>3. Action-run hook</strong></td>\n        <td><code>.antiloki/skills/audit/law-hook-*.mjs</code></td>\n        <td>\"AI tries the forbidden action anyway\" — hook returns allowed=false, dispatch throws</td>\n        <td><code>npx node --test .antiloki/skills/audit/law-hooks.test.mjs</code></td>\n      </tr>\n      <tr>\n        <td><strong>4. DB trigger</strong></td>\n        <td>SQLite triggers on <code>sacred_timeline</code></td>\n        <td>\"AI tries to forge an audit row directly\" — UPDATE/DELETE blocked + sha chain enforced</td>\n        <td><code>node bin/antiloki.mjs --unsafe-skill verify-chain '{}'</code></td>\n      </tr>\n      <tr>\n        <td><strong>5. Consistency-scan</strong></td>\n        <td>pre-commit hook · <code>scripts/pre-commit-consistency.mjs</code></td>\n        <td>\"AI bypassed everything earlier\" — 16-dimension scan catches drift before commit</td>\n        <td><code>node scripts/pre-commit-consistency.mjs</code></td>\n      </tr>\n    </table>\n    <h3>Hard-law enforcement is fail-closed (2026-06-10)</h3>\n    <div class=\"panel good\">\n      <p style=\"margin-top:0\">When <code>action-run</code> meets a HARD law whose <code>enforcement_hook</code> is <strong>missing or unloadable</strong>, the dispatch now throws <code>PERMISSION_DENIED</code> and logs a <code>law_violations</code> row with <code>outcome='blocked'</code> — instead of the previous fail-open behavior (<code>allowed_exception</code>, dispatch proceeds). A hard law that cannot be enforced refuses rather than pretends.</p>\n      <ul>\n        <li><strong>Recovery hatches:</strong> <code>ANTILOKI_LAW_FAILOPEN=1</code> is the narrow hatch — it only restores the old behavior for the <em>missing-hook</em> case (working hooks still enforce; the escape is logged once per session). <code>ANTILOKI_SKIP_LAW_ENFORCEMENT=true</code> remains the full bypass.</li>\n        <li><strong>No negative hook caching:</strong> only successful hook resolutions are cached. A hook file that lands mid-session is picked up by every long-lived process (BE, sandbox daemon) on its next dispatch — a miss is re-resolved every time.</li>\n        <li><strong>Gate at law creation:</strong> <code>law-add</code> refuses a hard law whose <code>enforcement_hook</code> does not resolve, unless the caller passes <code>allow_pending_hook: true</code> to defer deliberately.</li>\n        <li><strong>Scan backstop:</strong> consistency-scan dimension D3 (<code>LAW_HOOK_RESOLUTION</code>) flags any unresolvable hard-law hook as <span class=\"tag hard\">critical</span>, so the pre-commit hook catches it even if no dispatch ran.</li>\n      </ul>\n    </div>\n    <h3>New scan dimension: D16 ALLOWLIST_DISPATCH_DRIFT</h3>\n    <p>Diffs every action name referenced by <code>antiloki-BE/src</code> source against the lone-dispatch <code>BOOTSTRAP_ALLOWLIST</code>. An action the BE dispatches but the allowlist doesn't know about would 403 in block mode at runtime — D16 surfaces the drift at scan time instead (warning severity; god-token-gated and driver-signed paths are excluded).</p>\n  ";

// defenseMatrixSection — fully static prose + table.
const DEFENSE_MATRIX_SECTION: &str = "\n    <h2>5. Defense matrix — lie pattern → catching layer → proof</h2>\n    <p>For each documented failure mode, where it gets caught now and the exact command to verify the gate is wired.</p>\n    <table>\n      <tr><th>lie pattern</th><th>catches at layer</th><th>verify it blocks</th></tr>\n      <tr>\n        <td><strong>\"I built X\" with no command to verify</strong></td>\n        <td>1. Document (<code>CLAIM_REQUIRES_VERIFICATION</code> soft law) + 2. RAG-surface</td>\n        <td><code>node bin/antiloki.mjs --unsafe-skill law-surface-for-context '{\"action\":\"any\",\"agent\":\"dev\"}' | grep CLAIM</code></td>\n      </tr>\n      <tr>\n        <td><strong>Manually setting qa_pass to unblock work</strong></td>\n        <td>3. Hook (<code>law-hook-no-direct-audit-write</code>) + 4. DB trigger</td>\n        <td>Hook test: <code>npx node --test .antiloki/skills/audit/law-hooks.test.mjs --test-name-pattern='no-direct-audit-write'</code></td>\n      </tr>\n      <tr>\n        <td><strong>git revert / git reset --hard from a action</strong></td>\n        <td>3. Hook (<code>law-hook-no-destructive-git</code>)</td>\n        <td><code>node bin/antiloki.mjs board-keeper.card.add '{\"slug\":\"X\",\"title\":\"x\",\"body\":\"git push --force\"}'</code> → PERMISSION_DENIED</td>\n      </tr>\n      <tr>\n        <td><strong>Direct UPDATE bypassing with-audit</strong></td>\n        <td>3. Hook (<code>law-hook-mutation-via-with-audit</code>) + audit-gap canary test</td>\n        <td><code>node bin/antiloki.mjs --unsafe-skill detect-bypass '{}'</code></td>\n      </tr>\n      <tr>\n        <td><strong>Committing .env / API key / private key</strong></td>\n        <td>3. Hook (<code>law-hook-no-secrets-in-commit</code>) + 5. consistency-scan D5</td>\n        <td>Hook test catches AWS / OpenAI / Anthropic / GitHub / private-key patterns</td>\n      </tr>\n      <tr>\n        <td><strong>\"Card marked done\" without DB mutation</strong></td>\n        <td>5. consistency-scan D12 (<code>CARD_STATUS_COVERAGE</code>)</td>\n        <td><code>node bin/antiloki.mjs --unsafe-skill consistency-scan '{}' | grep CARD_STATUS_COVERAGE</code></td>\n      </tr>\n      <tr>\n        <td><strong>CLAUDE.md / AGENTS.md drift from laws.sqlite</strong></td>\n        <td>5. consistency-scan D11 (<code>INSTRUCTION_DOCS_SYNC</code>)</td>\n        <td><code>node bin/antiloki.mjs --unsafe-skill consistency-scan '{}' | grep INSTRUCTION_DOCS_SYNC</code></td>\n      </tr>\n      <tr>\n        <td><strong>Auto-commit fills git log while real code stays uncommitted</strong></td>\n        <td>3. Hook (<code>law-hook-no-drift-on-audit-commit</code>)</td>\n        <td>Drift detection is on by default. Bypass: <code>ANTILOKI_ALLOW_DRIFT=true</code></td>\n      </tr>\n    </table>\n  ";

// consequencesSection — fully static prose.
const CONSEQUENCES_SECTION: &str = "\n    <h2>6. The consequences of trying to lie</h2>\n    <p>If an AI attempts any of the above and is caught at layer 3 or 4:</p>\n    <ol>\n      <li>The action-run dispatch throws <code>PERMISSION_DENIED</code> with the law slug, the hook name, the regex/token that matched, and a <code>suggested_path</code>.</li>\n      <li>A row is inserted into <code>law_violations</code> with <code>outcome='blocked'</code>, naming the attempting agent.</li>\n      <li>The mutation never lands. <code>sacred_timeline</code> chain stays clean.</li>\n    </ol>\n    <p>If caught at layer 5 (pre-commit):</p>\n    <ol>\n      <li>The pre-commit hook exits non-zero with the critical findings listed.</li>\n      <li>Git refuses the commit. The user sees the exact regen command.</li>\n      <li>Bypass requires <code>--no-verify</code> — a deliberate, visible action.</li>\n    </ol>\n    <div class=\"panel warn\">\n      <strong>What the substrate cannot do:</strong> stop an AI from generating untrue prose <em>about</em> the system. It can only stop the prose from translating into harmful state. The discipline (\"every claim has a verification command\") is enforced socially via the document layer + correction memories; the technical layers catch the falsifications that touch the DB or git.\n    </div>\n  ";

// svgDispatchFlow() — no interpolation at all, always the same string. Used inside flowSection.
const SVG_DISPATCH_FLOW: &str = "\n<svg class=\"diag\" viewBox=\"0 0 860 460\" xmlns=\"http://www.w3.org/2000/svg\">\n  <text x=\"430\" y=\"22\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"13\" font-weight=\"600\">Action dispatch — every invocation passes through these gates in order</text>\n\n  <!-- Step 1: agent/auth -->\n  <rect x=\"280\" y=\"50\" width=\"300\" height=\"44\" rx=\"4\" fill=\"#1f2530\" stroke=\"#58a6ff\" stroke-width=\"1.3\"/>\n  <text x=\"430\" y=\"70\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"13\" font-weight=\"600\">1. Agent + auth check</text>\n  <text x=\"430\" y=\"86\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">HMAC-verified token · agent_actions grant</text>\n\n  <!-- Step 2: law-check -->\n  <rect x=\"280\" y=\"110\" width=\"300\" height=\"44\" rx=\"4\" fill=\"#1f2530\" stroke=\"#3fb950\" stroke-width=\"1.3\"/>\n  <text x=\"430\" y=\"130\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"13\" font-weight=\"600\">2. Law-check loads candidates</text>\n  <text x=\"430\" y=\"146\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">All HARD laws with enforcement_hook set</text>\n\n  <!-- Step 3: hooks loop -->\n  <rect x=\"280\" y=\"170\" width=\"300\" height=\"44\" rx=\"4\" fill=\"#1f2530\" stroke=\"#bc8cff\" stroke-width=\"1.3\"/>\n  <text x=\"430\" y=\"190\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"13\" font-weight=\"600\">3. For each law → invoke hook</text>\n  <text x=\"430\" y=\"206\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">hook({ action_name, input_data }) → { allowed, reason }</text>\n\n  <!-- Branch: blocked or proceed -->\n  <line x1=\"430\" y1=\"214\" x2=\"430\" y2=\"240\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n  <polygon points=\"426,238 434,238 430,244\" fill=\"#30363d\"/>\n\n  <rect x=\"80\" y=\"250\" width=\"280\" height=\"44\" rx=\"4\" fill=\"#1f2530\" stroke=\"#f85149\" stroke-width=\"1.3\"/>\n  <text x=\"220\" y=\"270\" text-anchor=\"middle\" fill=\"#f85149\" font-family=\"ui-monospace, monospace\" font-size=\"13\" font-weight=\"600\">allowed=false ⇒ THROW</text>\n  <text x=\"220\" y=\"286\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">PERMISSION_DENIED · violation logged</text>\n\n  <rect x=\"500\" y=\"250\" width=\"280\" height=\"44\" rx=\"4\" fill=\"#1f2530\" stroke=\"#3fb950\" stroke-width=\"1.3\"/>\n  <text x=\"640\" y=\"270\" text-anchor=\"middle\" fill=\"#3fb950\" font-family=\"ui-monospace, monospace\" font-size=\"13\" font-weight=\"600\">allowed=true ⇒ proceed</text>\n  <text x=\"640\" y=\"286\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">to step 4 (RAG + steps execution)</text>\n\n  <line x1=\"430\" y1=\"244\" x2=\"220\" y2=\"250\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n  <line x1=\"430\" y1=\"244\" x2=\"640\" y2=\"250\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n\n  <!-- Step 4: RAG + steps -->\n  <rect x=\"280\" y=\"310\" width=\"300\" height=\"44\" rx=\"4\" fill=\"#1f2530\" stroke=\"#d29922\" stroke-width=\"1.3\"/>\n  <text x=\"430\" y=\"330\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"13\" font-weight=\"600\">4. RAG-surface soft laws + run steps</text>\n  <text x=\"430\" y=\"346\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">law-surface injects 5 relevant soft laws into runCtx</text>\n\n  <line x1=\"640\" y1=\"294\" x2=\"430\" y2=\"310\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n  <line x1=\"430\" y1=\"354\" x2=\"430\" y2=\"380\" stroke=\"#30363d\" stroke-width=\"1.5\"/>\n  <polygon points=\"426,378 434,378 430,384\" fill=\"#30363d\"/>\n\n  <!-- Step 5: with-audit (DB trigger layer) -->\n  <rect x=\"280\" y=\"390\" width=\"300\" height=\"44\" rx=\"4\" fill=\"#1f2530\" stroke=\"#58a6ff\" stroke-width=\"1.3\"/>\n  <text x=\"430\" y=\"410\" text-anchor=\"middle\" fill=\"#e6edf3\" font-family=\"ui-monospace, monospace\" font-size=\"13\" font-weight=\"600\">5. with-audit writes sacred_timeline</text>\n  <text x=\"430\" y=\"426\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">DB triggers reject UPDATE/DELETE/forged sha</text>\n\n  <text x=\"430\" y=\"454\" text-anchor=\"middle\" fill=\"#8b949e\" font-family=\"ui-monospace, monospace\" font-size=\"11\">Any failure at steps 1/2/3/5 throws BEFORE the call mutates anything.</text>\n</svg>";

fn tldr_section(laws_len: usize, hard_len: usize, constitutional_len: usize, unique_hooks_len: usize, actions: i64, agents: i64, chain_len: i64) -> String {
    format!(
        "\n    <div class=\"panel good\">\n      <strong>TL;DR — currently active enforcement</strong>\n      <ul>\n        <li><span class=\"tag good\">{laws_len}</span> active laws in <code>laws.sqlite</code> · <span class=\"tag hard\">{hard_len} HARD</span> · {constitutional_len} constitutional</li>\n        <li><span class=\"tag good\">{unique_hooks_len}</span> live action-run hooks blocking dispatch when violated</li>\n        <li><span class=\"tag good\">{actions}</span> registered actions · <span class=\"tag good\">{agents}</span> agents · <span class=\"tag good\">{chain_len}</span> immutable audit events</li>\n        <li><span class=\"tag good\">16</span> consistency-scan dimensions enforced by pre-commit hook</li>\n        <li><strong>Fail-closed since 2026-06-10:</strong> a hard law whose enforcement hook is missing or unloadable BLOCKS dispatch instead of silently allowing it</li>\n        <li><strong>Document layer:</strong> AGENTS.md is the single source of truth; CLAUDE.md is a thin shim that imports it (per <a href=\"https://code.claude.com/docs/en/memory\" style=\"color:var(--accent)\">code.claude.com/docs/en/memory</a>)</li>\n      </ul>\n    </div>\n  "
    )
}

fn trust_problem_section(corrections: &[Value]) -> String {
    let inner = if !corrections.is_empty() {
        let rows: String = corrections
            .iter()
            .map(|c| {
                let id = tmpl(c.get("id"));
                // (c.content_preview || '').slice(0, 100) — JS `||` falls back to '' on any falsy value.
                let preview_raw = if truthy(c.get("content_preview")) {
                    match c.get("content_preview") {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => js_string(other),
                        None => String::new(),
                    }
                } else {
                    String::new()
                };
                let preview = esc_str(&preview_raw.chars().take(100).collect::<String>());
                let created = match c.get("created_at") {
                    Some(v) if !v.is_null() => tmpl(Some(v)),
                    _ => "-".to_string(),
                };
                format!("<tr><td><code>#{id}</code></td><td>{preview}…</td><td class=\"muted\">{created}</td></tr>")
            })
            .collect::<Vec<_>>()
            .join("");
        format!(
            "\n    <h3>Documented correction memories (live, from this DB)</h3>\n    <table>\n      <tr><th>id</th><th>preview</th><th>created</th></tr>\n      {rows}\n    </table>\n    <p class=\"muted\">Re-fetch: <code>node bin/antiloki.mjs --unsafe-skill memory-find '{{\"query\":\"correction\",\"memory_type\":\"correction\"}}'</code></p>\n    "
        )
    } else {
        String::new()
    };
    format!(
        "\n    <h2>1. The trust problem</h2>\n    <p>An AI assistant generates plausible-looking text whether or not it reflects reality. Without instrumentation, a session can produce:</p>\n    <div class=\"lie\">\"I built X\" — when X was described in prose but never written to disk.</div>\n    <div class=\"lie\">\"All N items filed\" — when only the prose describes the items, and the count was never verified.</div>\n    <div class=\"lie\">\"Card marked done\" — when the card status was updated in conversation but never in the DB.</div>\n    <div class=\"lie\">\"qa_pass set\" — when no QA actually ran, the row was just inserted manually to unblock work.</div>\n    <p>Each of these has happened in this repository's history (documented as <strong>correction memories</strong> in <code>memory.sqlite</code>). The substrate's job is to make these patterns either impossible or visible.</p>\n    {inner}\n  "
    )
}

fn flow_section(unique_hooks_len: usize, laws_with_hook: &[&Value]) -> String {
    let rows: String = laws_with_hook
        .iter()
        .map(|l| {
            let tag_class = if l.get("enforcement").and_then(|v| v.as_str()) == Some("hard") { "hard" } else { "soft" };
            format!(
                "<tr><td><code>{}</code></td><td><span class=\"tag {tag_class}\">{}</span> {}</td><td><code>{}</code></td></tr>",
                esc_html(l.get("slug").unwrap_or(&Value::Null)),
                esc_html(l.get("enforcement").unwrap_or(&Value::Null)),
                esc_html(l.get("kind").unwrap_or(&Value::Null)),
                esc_html(l.get("enforcement_hook").unwrap_or(&Value::Null)),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        "\n    <h2>4. Action-dispatch workflow</h2>\n    <p>Every action invocation (whether from CLI, MCP, or a chained action) flows through <code>action-run</code>. The hook stack runs <strong>before</strong> any side effect can happen.</p>\n    {SVG_DISPATCH_FLOW}\n    <h3>Live evidence: the {unique_hooks_len} hooks currently wired</h3>\n    <table>\n      <tr><th>law</th><th>kind</th><th>enforcement_hook</th></tr>\n      {rows}\n    </table>\n    <div class=\"proof\">Re-fetch this list: <code>node bin/antiloki.mjs --unsafe-skill law-check '{{}}'</code></div>\n  "
    )
}

fn repro_section(unique_hooks_len: usize) -> String {
    format!(
        "\n    <h2>7. Reproducibility — run all of this yourself</h2>\n    <pre><code># All hook unit tests pass\nnpx node --test .antiloki/skills/audit/law-hooks.test.mjs\n\n# End-to-end: a forbidden destructive-git command is blocked\nnode bin/antiloki.mjs board-keeper.card.add '{{\"slug\":\"X\",\"title\":\"x\",\"body\":\"git push --force origin\"}}'\n# → Error [PERMISSION_DENIED]: law 'HISTORY_IS_SACRED_NO_REVERT' denied\n\n# All {unique_hooks_len} hooks are loaded by law-check\nnode bin/antiloki.mjs --unsafe-skill law-check '{{}}'\n\n# Pre-commit hook is installed and blocks on critical findings\nls -la .git/hooks/pre-commit\nnode scripts/pre-commit-consistency.mjs   # exits 0 when clean\nmv CLAUDE.md /tmp/ && node scripts/pre-commit-consistency.mjs ; mv /tmp/CLAUDE.md .\n# → exits 1 with the missing-doc finding\n\n# Re-render this very document\nnode bin/antiloki.mjs system.docs.defense '{{}}' > docs/substrate-defense.html\n</code></pre>\n    <p class=\"muted\">Every command above can be copy-pasted into a fresh terminal. The fact that this document was generated from the live substrate means re-running <code>system.docs.defense</code> always returns the current truth — if a layer is removed, the corresponding row disappears.</p>\n  "
    )
}

pub fn substrateDefenseHtml(_input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('substrate-defense-html','db') when no db — the db lane always
    // injects one, so the guard-throw path is not exercised (fixtures cover success only).

    let laws = crate::db_json::query_rows(
        db,
        "SELECT * FROM laws.laws WHERE superseded_by IS NULL AND enforcement != 'archived' ORDER BY kind, slug",
        &[],
    )?
    .as_array()
    .cloned()
    .unwrap_or_default();
    let hard_laws: Vec<&Value> = laws.iter().filter(|l| l.get("enforcement").and_then(|v| v.as_str()) == Some("hard")).collect();
    let laws_with_hook: Vec<&Value> = laws.iter().filter(|l| truthy(l.get("enforcement_hook"))).collect();
    let constitutional_laws: Vec<&Value> = laws.iter().filter(|l| l.get("kind").and_then(|v| v.as_str()) == Some("constitutional")).collect();

    let actions_count = first_count(&crate::db_json::query_rows(db, "SELECT COUNT(*) AS c FROM actions", &[])?);
    let agents_count = first_count(&crate::db_json::query_rows(db, "SELECT COUNT(*) AS c FROM agents", &[])?);

    // count audit events (chain length)
    let chain_len = crate::db_json::query_rows(db, "SELECT COUNT(*) AS c FROM sacred_timeline", &[])
        .ok()
        .map(|v| first_count(&v))
        .unwrap_or(0);

    // correction memories (DPO pairs documenting past lies) — memory.sqlite may not be attached in
    // test contexts, and the real schema's column is `type` (not `memory_type`), so this errors and
    // is caught in production too; the twin transcribes the SAME query so both lanes fail the same way.
    let corrections: Vec<Value> = crate::db_json::query_rows(
        db,
        "SELECT id, content_preview, created_at FROM memory.memories WHERE memory_type = 'correction' ORDER BY id DESC LIMIT 10",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();

    // pull hook skill names to prove they exist — Array.from(new Set(...)) keeps first-seen order.
    let mut seen: HashSet<String> = HashSet::new();
    let unique_hooks: Vec<String> = laws_with_hook
        .iter()
        .filter_map(|l| l.get("enforcement_hook").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .filter(|h| seen.insert(h.clone()))
        .collect();

    let header_section = HEADER_SECTION;
    let tldr_section = tldr_section(laws.len(), hard_laws.len(), constitutional_laws.len(), unique_hooks.len(), actions_count, agents_count, chain_len);
    let trust_problem_section = trust_problem_section(&corrections);
    let flow_section = flow_section(unique_hooks.len(), &laws_with_hook);
    let repro_section = repro_section(unique_hooks.len());

    let html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"UTF-8\">\n<title>antiloki substrate — defense-in-depth</title>\n<style>{CSS}</style>\n</head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"./index.html\" style=\"color:#58a6ff;text-decoration:none;\">← antiloki docs</a></nav>\n<div class=\"wrap\">\n{header_section}\n{tldr_section}\n{trust_problem_section}\n{SUBSTRATE_SECTION}\n{LAYERS_SECTION}\n{flow_section}\n{DEFENSE_MATRIX_SECTION}\n{CONSEQUENCES_SECTION}\n{repro_section}\n<hr>\n<p class=\"muted\" style=\"text-align:center\">antiloki-v3 · auto-generated from <code>laws.sqlite</code> at render time · {laws_len} laws · {hooks_len} hooks · {actions_count} actions</p>\n</div>\n</body>\n</html>",
        laws_len = laws.len(),
        hooks_len = unique_hooks.len(),
    );

    Ok(Value::String(html))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SCHEMA: &str = "ATTACH ':memory:' AS laws; ATTACH ':memory:' AS memory;
        CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, title TEXT, statement TEXT, rationale TEXT, enforcement TEXT, enforcement_hook TEXT, trigger_sql TEXT, established_at TEXT, established_by TEXT, source_card TEXT, source_event INTEGER, source_memory INTEGER, superseded_by INTEGER, superseded_at TEXT);
        CREATE TABLE actions (id INTEGER PRIMARY KEY, name TEXT);
        CREATE TABLE agents (id INTEGER PRIMARY KEY, name TEXT);
        CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_id TEXT);
        CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, memory_type TEXT, content_preview TEXT, created_at TEXT);";

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn empty_substrate_renders_zero_counts_and_no_correction_table() {
        let c = conn();
        let got = substrateDefenseHtml(&json!({}), &c).unwrap();
        let s = got.as_str().unwrap();
        assert!(s.starts_with("<!DOCTYPE html>\n<html lang=\"en\">\n"));
        assert!(s.ends_with("</div>\n</body>\n</html>"));
        assert!(s.contains("<title>antiloki substrate — defense-in-depth</title>"));
        assert!(s.contains("<span class=\"tag good\">0</span> active laws in <code>laws.sqlite</code> · <span class=\"tag hard\">0 HARD</span> · 0 constitutional"));
        assert!(s.contains("<span class=\"tag good\">0</span> registered actions · <span class=\"tag good\">0</span> agents · <span class=\"tag good\">0</span> immutable audit events"));
        assert!(!s.contains("Documented correction memories"));
        assert!(s.contains("<h3>Live evidence: the 0 hooks currently wired</h3>\n    <table>\n      <tr><th>law</th><th>kind</th><th>enforcement_hook</th></tr>\n      \n    </table>"));
        assert!(s.contains("# All 0 hooks are loaded by law-check"));
        assert!(s.contains("· 0 laws · 0 hooks · 0 actions</p>"));
    }

    #[test]
    fn throws_without_db_is_not_exercised_but_full_dataset_renders_all_dynamic_sections() {
        let c = conn();
        c.execute_batch(
            "INSERT INTO laws.laws (id,slug,kind,enforcement,enforcement_hook,superseded_by) VALUES
              (1,'LAW-A','constitutional','hard','hook-a',NULL),
              (2,'LAW-E','constitutional','hard','hook-a',NULL),
              (3,'LAW-B','constraint','hard',NULL,NULL),
              (4,'LAW-D','decision','soft',NULL,NULL),
              (5,'LAW-C','procedural','soft','hook-b',NULL),
              (6,'LAW-F','principle','archived',NULL,NULL),
              (7,'LAW-G','constitutional','hard','hook-c',1);
            INSERT INTO actions (id,name) VALUES (1,'a1'),(2,'a2'),(3,'a3');
            INSERT INTO agents (id,name) VALUES (1,'g1'),(2,'g2');
            INSERT INTO sacred_timeline (id,entity_id) VALUES (1,'1'),(2,'2'),(3,'3'),(4,'4'),(5,'5');
            INSERT INTO memory.memories (id,memory_type,content_preview,created_at) VALUES
              (9,'lesson','not a correction','2026-01-01 00:00:00'),
              (10,'correction','A short preview','2026-01-01 10:00:00'),
              (11,'correction','<div class=\"x\">A & B</div>zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz',NULL);",
        )
        .unwrap();

        let got = substrateDefenseHtml(&json!({}), &c).unwrap();
        let s = got.as_str().unwrap();

        // TL;DR: 5 active laws (LAW-F archived + LAW-G superseded excluded), 3 hard, 2 constitutional
        assert!(s.contains("<span class=\"tag good\">5</span> active laws in <code>laws.sqlite</code> · <span class=\"tag hard\">3 HARD</span> · 2 constitutional"));
        assert!(s.contains("<span class=\"tag good\">2</span> live action-run hooks blocking dispatch when violated"));
        assert!(s.contains("<span class=\"tag good\">3</span> registered actions · <span class=\"tag good\">2</span> agents · <span class=\"tag good\">5</span> immutable audit events"));

        // corrections: ordered id DESC, escaped + sliced to 100 chars + ellipsis, null created_at → '-'
        assert!(s.contains("<h3>Documented correction memories (live, from this DB)</h3>"));
        assert!(s.contains("<tr><td><code>#11</code></td><td>&lt;div class=&quot;x&quot;&gt;A &amp; B&lt;/div&gt;zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz…</td><td class=\"muted\">-</td></tr><tr><td><code>#10</code></td><td>A short preview…</td><td class=\"muted\">2026-01-01 10:00:00</td></tr>"));
        assert!(!s.contains("#9")); // 'lesson' type excluded by the WHERE clause

        // flow section: kind-then-slug ordering, hook-a deduped across LAW-A/LAW-E
        assert!(s.contains("<h3>Live evidence: the 2 hooks currently wired</h3>"));
        assert!(s.contains("<tr><td><code>LAW-A</code></td><td><span class=\"tag hard\">hard</span> constitutional</td><td><code>hook-a</code></td></tr><tr><td><code>LAW-E</code></td><td><span class=\"tag hard\">hard</span> constitutional</td><td><code>hook-a</code></td></tr><tr><td><code>LAW-C</code></td><td><span class=\"tag soft\">soft</span> procedural</td><td><code>hook-b</code></td></tr>"));

        assert!(s.contains("# All 2 hooks are loaded by law-check"));
        assert!(s.contains("· 5 laws · 2 hooks · 3 actions</p>"));
    }

    #[test]
    fn missing_sacred_timeline_and_memory_tables_fall_back_to_zero_and_empty() {
        // No sacred_timeline table and no memory ATTACH at all — both catch branches fire.
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS laws;
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER);
             CREATE TABLE actions (id INTEGER PRIMARY KEY);
             CREATE TABLE agents (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        let got = substrateDefenseHtml(&json!({}), &c).unwrap();
        let s = got.as_str().unwrap();
        assert!(s.contains("<span class=\"tag good\">0</span> immutable audit events"));
        assert!(!s.contains("Documented correction memories"));
    }

    #[test]
    fn esc_str_escapes_amp_lt_gt_quot_in_order() {
        assert_eq!(esc_str("<a href=\"x\">A & B</a>"), "&lt;a href=&quot;x&quot;&gt;A &amp; B&lt;/a&gt;");
    }
}
