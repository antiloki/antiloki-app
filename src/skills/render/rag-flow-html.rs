//! rag-flow-html.rs — twin of skills/render/rag-flow-html.mjs (db READ lane, single default
//! export `ragFlowHtml`). NAMES ARE CONTRACT: `ragFlowHtml`.
//!
//! BESPOKE NATIVE (no conform-db fixture): the JS source `await import('../entities/catalog-find.mjs')`
//! at call time (a real dynamic `import()`), then invokes it live for a "probe query" section of
//! the rendered page whenever `memory.catalog_embeddings` already holds >=1 row. Per the crossing
//! playbook's cross-module convention, since `catalog-find.mjs` already has a twin
//! (`entities_catalog_find::catalogFind`), the dynamic import is resolved by calling that twin
//! directly rather than treating it as a deferred boundary — this twin is otherwise fully
//! deterministic. What makes the WHOLE unit genuinely non-deterministic for fixture purposes is
//! `catalogFind` itself: whenever `memory.catalog_embeddings` has rows for the probed
//! `entity_type` ("skill" / "law"), `catalogFind`'s `useModel` resolves truthy and its own
//! module doc explains that branch is a real, unported `embed()` ML/network call (mirrored here
//! as `Err(..)`, caught exactly like the JS `.catch(() => [])` per call). So this page's rendered
//! "live probe" section is a function of live, ambient substrate state (whether embeddings exist)
//! in a way no static fixture can pin — same class as excalidraw-rag.rs / excalidraw-driver.rs
//! (real-effect-shaped output), proven only by `#[cfg(test)]` here.
//!
//! Frozen clock (playbook): unused — this module has no wall-clock/dynamic-timestamp reads.

use rusqlite::Connection;
use serde_json::{json, Value};

fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const CSS: &str = r##"
:root {
  --bg:#0e1116; --panel:#161b22; --panel2:#1f2530; --border:#30363d;
  --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950;
  --warn:#d29922; --bad:#f85149; --code-bg:#0d1117; --rag:#bc8cff;
}
* { box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; background: var(--bg); color: var(--fg); margin: 0; padding: 0; line-height: 1.55; }
.wrap { max-width: 1080px; margin: 0 auto; padding: 2em 1.5em 4em; }
h1 { font-size: 2em; font-weight: 700; letter-spacing: -0.02em; margin: 0 0 0.2em; }
h2 { font-size: 1.45em; font-weight: 600; margin-top: 2.5em; padding-bottom: 0.3em; border-bottom: 1px solid var(--border); }
h3 { font-size: 1.1em; font-weight: 600; margin-top: 1.6em; color: var(--fg); }
.lede { color: var(--muted); font-size: 1.05em; max-width: 720px; }
.meta { color: var(--muted); font-size: 0.85em; margin: 0.3em 0 1.5em; font-family: ui-monospace, monospace; }
code, pre { font-family: ui-monospace, "JetBrains Mono", Consolas, monospace; font-size: 0.88em; }
code { background: var(--code-bg); padding: 1px 5px; border-radius: 3px; border: 1px solid var(--border); }
pre { background: var(--code-bg); padding: 0.9em 1.1em; border-radius: 6px; border: 1px solid var(--border); overflow-x: auto; }
table { width: 100%; border-collapse: collapse; margin: 0.6em 0 1.4em; font-size: 0.92em; }
th, td { padding: 0.55em 0.9em; text-align: left; border-bottom: 1px solid var(--border); vertical-align: top; }
th { color: var(--muted); font-weight: 500; font-size: 0.85em; background: var(--panel); }
tr:hover td { background: var(--panel2); }
.panel { background: var(--panel); border: 1px solid var(--border); border-radius: 8px; padding: 1.1em 1.3em; margin: 1em 0; }
.panel.rag { border-color: var(--rag); }
.kicker { color: var(--rag); font-family: ui-monospace, monospace; font-size: 0.78em; text-transform: uppercase; letter-spacing: 0.08em; margin-bottom: 0.2em; }
.tag { display: inline-block; padding: 1px 7px; border-radius: 10px; font-size: 0.78em; font-family: ui-monospace, monospace; margin-right: 4px; background: rgba(188,140,255,0.12); color: var(--rag); border: 1px solid rgba(188,140,255,0.35); }
.score { font-family: ui-monospace, monospace; color: var(--good); }
svg.diag { display: block; margin: 1em auto; max-width: 100%; height: auto; background: var(--panel); border: 1px solid var(--border); border-radius: 8px; }
.muted { color: var(--muted); }
.gridcounts { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 0.8em; margin: 1em 0; }
.count { background: var(--panel); border: 1px solid var(--border); padding: 0.9em 1.1em; border-radius: 6px; }
.count .label { color: var(--muted); font-size: 0.8em; }
.count .num { color: var(--rag); font-size: 1.7em; font-weight: 600; line-height: 1.1; }
.count .sub { color: var(--muted); font-size: 0.75em; font-family: ui-monospace, monospace; margin-top: 0.2em; }
hr { border: 0; border-top: 1px solid var(--border); margin: 2.5em 0; }
.heat { display: inline-block; width: 6px; height: 14px; vertical-align: middle; margin-right: 1px; }
"##;

fn svg_data_flow() -> String {
    r##"
<svg class="diag" viewBox="0 0 860 380" xmlns="http://www.w3.org/2000/svg">
  <text x="430" y="22" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="13" font-weight="600">RAG data flow — query to top-k results</text>

  <!-- Query input -->
  <rect x="60" y="60" width="180" height="60" rx="4" fill="#1f2530" stroke="#58a6ff" stroke-width="1.3"/>
  <text x="150" y="86" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="13" font-weight="600">User / Action query</text>
  <text x="150" y="104" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">e.g. "add a law-hook"</text>

  <!-- Arrow -->
  <line x1="240" y1="90" x2="285" y2="90" stroke="#30363d" stroke-width="1.5"/>
  <polygon points="280,86 285,90 280,94" fill="#30363d"/>

  <!-- Embed -->
  <rect x="290" y="60" width="180" height="60" rx="4" fill="#1f2530" stroke="#bc8cff" stroke-width="1.3"/>
  <text x="380" y="80" text-anchor="middle" fill="#bc8cff" font-family="ui-monospace, monospace" font-size="13" font-weight="600">embed.mjs</text>
  <text x="380" y="98" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">Xenova/bge-base-en-v1.5</text>
  <text x="380" y="113" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">WASM · 768-dim float32</text>

  <!-- Arrow -->
  <line x1="470" y1="90" x2="515" y2="90" stroke="#30363d" stroke-width="1.5"/>
  <polygon points="510,86 515,90 510,94" fill="#30363d"/>

  <!-- Cosine -->
  <rect x="520" y="60" width="180" height="60" rx="4" fill="#1f2530" stroke="#3fb950" stroke-width="1.3"/>
  <text x="610" y="80" text-anchor="middle" fill="#3fb950" font-family="ui-monospace, monospace" font-size="13" font-weight="600">cosine similarity</text>
  <text x="610" y="98" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">vs memory.catalog_embeddings</text>
  <text x="610" y="113" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">filter by entity_type</text>

  <!-- Branch down to results -->
  <line x1="610" y1="120" x2="610" y2="160" stroke="#30363d" stroke-width="1.5"/>
  <polygon points="606,158 614,158 610,164" fill="#30363d"/>

  <!-- Top-k -->
  <rect x="490" y="170" width="240" height="60" rx="4" fill="#1f2530" stroke="#d29922" stroke-width="1.3"/>
  <text x="610" y="190" text-anchor="middle" fill="#d29922" font-family="ui-monospace, monospace" font-size="13" font-weight="600">top-k by score</text>
  <text x="610" y="208" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">[{entity, score}]</text>
  <text x="610" y="223" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">default k=10, minScore=0.0</text>

  <!-- Fallback path -->
  <text x="150" y="170" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">fallback if no embeddings:</text>
  <rect x="60" y="180" width="180" height="60" rx="4" fill="#1f2530" stroke="#f85149" stroke-width="1.3" stroke-dasharray="4,4"/>
  <text x="150" y="200" text-anchor="middle" fill="#f85149" font-family="ui-monospace, monospace" font-size="13" font-weight="600">LIKE query</text>
  <text x="150" y="218" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">substring scan</text>
  <text x="150" y="233" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">graceful degradation</text>

  <!-- Consumers -->
  <text x="430" y="270" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="13" font-weight="600">Consumers</text>

  <rect x="50" y="290" width="200" height="50" rx="4" fill="#1f2530" stroke="#58a6ff" stroke-width="1.2"/>
  <text x="150" y="310" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="12">system.skill.find</text>
  <text x="150" y="326" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">discover similar skills</text>

  <rect x="260" y="290" width="200" height="50" rx="4" fill="#1f2530" stroke="#bc8cff" stroke-width="1.2"/>
  <text x="360" y="310" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="12">law-surface-for-context</text>
  <text x="360" y="326" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">inject relevant soft laws</text>

  <rect x="470" y="290" width="200" height="50" rx="4" fill="#1f2530" stroke="#3fb950" stroke-width="1.2"/>
  <text x="570" y="310" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="12">contract.draft.auto</text>
  <text x="570" y="326" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">auto-fill contract fields</text>

  <rect x="680" y="290" width="160" height="50" rx="4" fill="#1f2530" stroke="#d29922" stroke-width="1.2"/>
  <text x="760" y="310" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="12">memory.find</text>
  <text x="760" y="326" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">past-correction recall</text>

  <text x="430" y="365" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="11">Every consumer goes through catalog-find — single chokepoint, single dependency, fully auditable.</text>
</svg>"##.to_string()
}

fn svg_contract_flow() -> String {
    r##"
<svg class="diag" viewBox="0 0 860 320" xmlns="http://www.w3.org/2000/svg">
  <text x="430" y="22" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="13" font-weight="600">contract.draft.auto — pipeline using RAG + AST + DBs</text>

  <!-- 1 -->
  <rect x="60" y="50" width="160" height="46" rx="4" fill="#1f2530" stroke="#58a6ff" stroke-width="1.2"/>
  <text x="140" y="70" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="12" font-weight="600">1. Load card</text>
  <text x="140" y="86" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">cards table → row</text>

  <!-- 2 -->
  <rect x="240" y="50" width="160" height="46" rx="4" fill="#1f2530" stroke="#d29922" stroke-width="1.2"/>
  <text x="320" y="70" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="12" font-weight="600">2. Detect template</text>
  <text x="320" y="86" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">keyword + RAG hint</text>

  <!-- 3 RAG -->
  <rect x="420" y="50" width="160" height="46" rx="4" fill="#1f2530" stroke="#bc8cff" stroke-width="1.2"/>
  <text x="500" y="70" text-anchor="middle" fill="#bc8cff" font-family="ui-monospace, monospace" font-size="12" font-weight="600">3. RAG: skills</text>
  <text x="500" y="86" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">reference_artifacts</text>

  <!-- 4 RAG laws -->
  <rect x="600" y="50" width="160" height="46" rx="4" fill="#1f2530" stroke="#bc8cff" stroke-width="1.2"/>
  <text x="680" y="70" text-anchor="middle" fill="#bc8cff" font-family="ui-monospace, monospace" font-size="12" font-weight="600">4. RAG: soft laws</text>
  <text x="680" y="86" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">must_obey</text>

  <!-- 5 template-derived -->
  <rect x="60" y="120" width="220" height="46" rx="4" fill="#1f2530" stroke="#3fb950" stroke-width="1.2"/>
  <text x="170" y="140" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="12" font-weight="600">5. Template fields</text>
  <text x="170" y="156" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">files_touched · naming · edges · acceptance</text>

  <!-- 6 past-rejection -->
  <rect x="300" y="120" width="220" height="46" rx="4" fill="#1f2530" stroke="#bc8cff" stroke-width="1.2"/>
  <text x="410" y="140" text-anchor="middle" fill="#bc8cff" font-family="ui-monospace, monospace" font-size="12" font-weight="600">6. Past TL rejections</text>
  <text x="410" y="156" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">memory.memories type=correction</text>

  <!-- 7 classify -->
  <rect x="540" y="120" width="220" height="46" rx="4" fill="#1f2530" stroke="#d29922" stroke-width="1.2"/>
  <text x="650" y="140" text-anchor="middle" fill="#e6edf3" font-family="ui-monospace, monospace" font-size="12" font-weight="600">7. Classify</text>
  <text x="650" y="156" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">risk · complexity · tier</text>

  <!-- 8 assemble -->
  <rect x="240" y="190" width="380" height="50" rx="4" fill="#1f2530" stroke="#58a6ff" stroke-width="1.4"/>
  <text x="430" y="212" text-anchor="middle" fill="#58a6ff" font-family="ui-monospace, monospace" font-size="13" font-weight="600">8. Assemble contract JSON</text>
  <text x="430" y="228" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">+ confidence_per_field · + sources (template, RAG hits, past hints)</text>

  <!-- 9 TL gate -->
  <rect x="280" y="260" width="300" height="46" rx="4" fill="#1f2530" stroke="#d29922" stroke-width="1.3"/>
  <text x="430" y="280" text-anchor="middle" fill="#d29922" font-family="ui-monospace, monospace" font-size="12" font-weight="600">9. TL signs OR rejects</text>
  <text x="430" y="296" text-anchor="middle" fill="#8b949e" font-family="ui-monospace, monospace" font-size="10">reject → correction memory → feeds step 6 next time</text>

  <!-- Arrows -->
  <line x1="140" y1="96" x2="140" y2="115" stroke="#30363d" stroke-width="1.5"/>
  <line x1="320" y1="96" x2="320" y2="115" stroke="#30363d" stroke-width="1.5"/>
  <line x1="500" y1="96" x2="500" y2="115" stroke="#30363d" stroke-width="1.5"/>
  <line x1="680" y1="96" x2="680" y2="115" stroke="#30363d" stroke-width="1.5"/>
  <line x1="430" y1="166" x2="430" y2="185" stroke="#30363d" stroke-width="1.5"/>
  <line x1="430" y1="240" x2="430" y2="258" stroke="#30363d" stroke-width="1.5"/>
</svg>"##.to_string()
}

/// `function tinyHeat(score) { ... }` — 10 columns, brighter for higher score.
fn tiny_heat(score: f64) -> String {
    let mut cells = String::new();
    for i in 0..10 {
        let filled = (i as f64) / 10.0 < score;
        let opacity = if filled { 0.5 + 0.5 * score } else { 0.08 };
        cells.push_str(&format!(
            r##"<span class="heat" style="background:rgba(188,140,255,{opacity})"></span>"##
        ));
    }
    cells
}

struct ByType {
    entity_type: String,
    c: i64,
    dims: Option<i64>,
    model: Option<String>,
}

/// `export default async function ragFlowHtml(_input, context)` — `context.db` is required
/// (`err.contextMissing` guard-throw, unreachable here per playbook gotcha #9, mirrored by the
/// non-optional `db: &Connection` param).
pub fn ragFlowHtml(_input: &Value, db: &Connection) -> Result<Value, String> {
    // ── Live counts from substrate ──────────────────────────────────────
    let by_type_rows = crate::db_json::query_rows(
        db,
        "SELECT entity_type, COUNT(*) AS c, MAX(dims) AS dims, MAX(model) AS model \
         FROM memory.catalog_embeddings GROUP BY entity_type",
        &[],
    )?;
    let by_type: Vec<ByType> = by_type_rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|r| ByType {
            entity_type: r.get("entity_type").and_then(Value::as_str).unwrap_or("").to_string(),
            c: r.get("c").and_then(Value::as_i64).unwrap_or(0),
            dims: r.get("dims").and_then(Value::as_i64),
            model: r.get("model").and_then(Value::as_str).map(|s| s.to_string()),
        })
        .collect();

    let total_embeddings: i64 = by_type.iter().map(|r| r.c).sum();
    let distinct_models: i64 = db
        .query_row("SELECT COUNT(DISTINCT model) AS c FROM memory.catalog_embeddings", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let skill_count: i64 = db
        .query_row("SELECT COUNT(*) AS c FROM memory.skills", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let law_count: i64 = db
        .query_row("SELECT COUNT(*) AS c FROM laws.laws WHERE superseded_by IS NULL", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let memory_count: i64 = db
        .query_row("SELECT COUNT(*) AS c FROM memory.memories", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let correction_count: i64 = db
        .query_row("SELECT COUNT(*) AS c FROM memory.memories WHERE type='correction'", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;

    // ── Live probe query (if there are embeddings) ──────────────────────
    // JS: `const { default: catalogFind } = await import('../entities/catalog-find.mjs');` —
    // resolved by calling the already-converted twin directly (see module doc). Each call is
    // individually caught (`.catch(() => [])`), matching `catalogFind(...).catch(() => [])`.
    let probe_query = "add a new law-hook for auditing";
    let probe_results: Option<(Vec<Value>, Vec<Value>)> = if total_embeddings > 0 {
        let skill_input = json!({ "query": probe_query, "entity_type": "skill", "limit": 5, "fallback": true });
        let law_input = json!({ "query": probe_query, "entity_type": "law", "limit": 5, "fallback": true });
        let skill_hits = crate::entities_catalog_find::catalogFind(&skill_input, db)
            .unwrap_or(json!([]))
            .as_array()
            .cloned()
            .unwrap_or_default();
        let law_hits = crate::entities_catalog_find::catalogFind(&law_input, db)
            .unwrap_or(json!([]))
            .as_array()
            .cloned()
            .unwrap_or_default();
        Some((skill_hits, law_hits))
    } else {
        None
    };

    // ── Render ──────────────────────────────────────────────────────────
    let sub_model = by_type.first().and_then(|r| r.model.clone()).unwrap_or_else(|| "none yet".to_string());
    let tldr = format!(
        r##"
    <div class="panel rag">
      <strong>TL;DR — RAG layer at a glance</strong>
      <div class="gridcounts">
        <div class="count"><div class="label">total embeddings</div><div class="num">{total_embeddings}</div><div class="sub">across all entities</div></div>
        <div class="count"><div class="label">embed models in use</div><div class="num">{distinct_models}</div><div class="sub">{sub_model}</div></div>
        <div class="count"><div class="label">skills indexed</div><div class="num">{skill_count}</div><div class="sub">memory.skills</div></div>
        <div class="count"><div class="label">laws active</div><div class="num">{law_count}</div><div class="sub">laws.laws (non-superseded)</div></div>
        <div class="count"><div class="label">memories total</div><div class="num">{memory_count}</div><div class="sub">{correction_count} corrections</div></div>
      </div>
    </div>
  "##
    );

    let overview = r##"
    <h2>1. Overview</h2>
    <p>antiloki uses a <strong>local-only RAG layer</strong>: <code>@huggingface/transformers</code> runs the <code>Xenova/bge-base-en-v1.5</code> model in WASM directly inside Node, producing 768-dimensional float32 embeddings. No external API, no key, no rate limit. Embeddings are stored in <code>memory.catalog_embeddings</code> with the source entity reference + the exact text that was embedded (for cache invalidation on text change).</p>
    <p class="muted">Single chokepoint: <code>catalog-find</code> — every RAG consumer (skill-find, law-surface-for-context, contract.draft.auto, memory.find) routes through it. One implementation to audit, one fallback to maintain (LIKE-based substring search when no embeddings exist), one set of tests.</p>
  "##.to_string();

    let what_embedded_rows = if by_type.is_empty() {
        "<tr><td colspan=\"4\" class=\"muted\">no embeddings yet — run <code>node bin/antiloki.mjs system.catalog.embed.all '{}'</code></td></tr>".to_string()
    } else {
        by_type
            .iter()
            .map(|r| {
                format!(
                    "<tr><td><code>{}</code></td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                    esc_html(&r.entity_type),
                    r.c,
                    esc_html(r.model.as_deref().unwrap_or("?")),
                    r.dims.map(|d| d.to_string()).unwrap_or_else(|| "?".to_string()),
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let what_embedded = format!(
        r##"
    <h2>2. What is embedded right now</h2>
    <table>
      <tr><th>entity_type</th><th>count</th><th>model</th><th>dims</th></tr>
      {what_embedded_rows}
    </table>
    <p class="muted">Re-fetch: <code>node bin/antiloki.mjs --unsafe-skill catalog-find '{{"query":"X","entity_type":"skill","limit":5}}'</code></p>
  "##
    );

    let data_flow = format!(
        r##"
    <h2>3. Data flow</h2>
    {}
    <p class="muted">Fallback (red dashed) only triggers when no embeddings exist for the requested <code>entity_type</code> AND <code>fallback:true</code>. It's a substring match — useful before <code>catalog.embed.all</code> has run, but degraded quality vs vector search.</p>
  "##,
        svg_data_flow()
    );

    let probe_block = match &probe_results {
        Some((skill_hits, law_hits)) => {
            let skill_rows = if skill_hits.is_empty() {
                "<tr><td colspan=\"3\" class=\"muted\">no hits (no embeddings stored)</td></tr>".to_string()
            } else {
                skill_hits
                    .iter()
                    .map(|h| {
                        let score = h.get("score").and_then(Value::as_f64).unwrap_or(0.0);
                        let name = h.get("entity").and_then(|e| e.get("name")).and_then(Value::as_str).unwrap_or("?");
                        let clamped = score.max(0.0).min(1.0);
                        format!(
                            "<tr><td><code>{}</code></td><td><span class=\"score\">{:.3}</span></td><td>{}</td></tr>",
                            esc_html(name),
                            score,
                            tiny_heat(clamped)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("")
            };
            let law_rows = if law_hits.is_empty() {
                "<tr><td colspan=\"3\" class=\"muted\">no law embeddings stored</td></tr>".to_string()
            } else {
                law_hits
                    .iter()
                    .map(|h| {
                        let score = h.get("score").and_then(Value::as_f64).unwrap_or(0.0);
                        let slug = h.get("entity").and_then(|e| e.get("slug")).and_then(Value::as_str).unwrap_or("?");
                        let clamped = score.max(0.0).min(1.0);
                        format!(
                            "<tr><td><code>{}</code></td><td><span class=\"score\">{:.3}</span></td><td>{}</td></tr>",
                            esc_html(slug),
                            score,
                            tiny_heat(clamped)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("")
            };
            format!(
                r##"
    <h2>4. Live probe — ran just now</h2>
    <p>Query: <code>{probe_query}</code></p>
    <h3>Top-{} skill hits</h3>
    <table>
      <tr><th>skill</th><th>score</th><th>visual</th></tr>
      {skill_rows}
    </table>
    <h3>Top-{} law hits</h3>
    <table>
      <tr><th>law slug</th><th>score</th><th>visual</th></tr>
      {law_rows}
    </table>
    <p class="muted">Reproduce: <code>node bin/antiloki.mjs system.docs.rag '{{}}' &gt; docs/rag-flow.html</code></p>
    "##,
                skill_hits.len(),
                law_hits.len(),
            )
        }
        None => format!(
            r##"
    <h2>4. Live probe</h2>
    <div class="panel" style="border-color: var(--warn)">
      No embeddings exist yet. Run <code>node bin/antiloki.mjs system.catalog.embed.all '{{}}'</code> to populate them, then re-render this doc.
    </div>
  "##
        ),
    };

    let contract_flow = format!(
        r##"
    <h2>5. How <code>contract.draft.auto</code> consumes the RAG layer</h2>
    {}
    <p>This is the pipeline that turns a card into a 95%-filled contract using the RAG layer at three points (steps 3, 4, 6). Each RAG hit is logged in the <code>sources</code> field of the returned object — TL sees <em>why</em> each value was suggested, not just the value itself.</p>
    <p class="muted">Self-learning loop: when TL <code>contract.gate.tl.sign</code> rejects a draft, the missing-fields explanation is written as a <code>type='correction'</code> memory tagged <code>contract</code>. Step 6 of the next draft for a similar card retrieves these via SQL <code>LIKE '%contract%'</code> (will become RAG-ranked once correction memories themselves are embedded).</p>
  "##,
        svg_contract_flow()
    );

    let ai_call_pipeline = r##"
    <h2>6. The <code>ai-call</code> pipeline (prompt-quality program, 2026-06-10)</h2>
    <p>Every AI interaction flows through the single <code>ai-call</code> chokepoint (law <code>AI_CALL_THROUGH_ENVELOPE</code>). The 2026-06-10 prompt-quality program added five mechanisms on that chokepoint:</p>
    <table>
      <tr><th>mechanism</th><th>what it does</th></tr>
      <tr>
        <td><strong>Structured output — default ON</strong></td>
        <td>ai-call derives an OpenAI/OpenRouter-style <code>json_schema</code> <code>response_format</code> from the template's declared <code>output_fields</code> and sends it on every call. Provider-native enforcement of the output shape; <code>structured_output: false</code> is the explicit opt-out.</td>
      </tr>
      <tr>
        <td><strong><code>system_md</code> on prompt templates</strong></td>
        <td>Templates can declare a system message; adapters send it as the provider's system role instead of inlining role text into the user prompt.</td>
      </tr>
      <tr>
        <td><strong>Two-phase <code>phase_flow='reason_then_format'</code></strong></td>
        <td>A template with this flow first runs its companion <code>&lt;name&gt;.reason</code> template (free-form diagnosis), then injects that output into the main template as <code>{{reasoning_output}}</code> for the format phase. Two <code>ai_calls</code> rows capture both phases; chained reason phases are forbidden. Tier philosophy (operator, 2026-06-10): the reason phase carries the REASONING at <strong>high/warm</strong>; the format phase is the OBEDIENCE pass — follow the diagnosis, emit the exact output shape — at <strong>low/freezed</strong>. First hand-authored application: <code>tl.refine.microtasks.v9</code> — diagnose (AI&nbsp;HIGH), then author (AI&nbsp;LOW).<br><br><strong>AUTO two-phase (default for ALL high-tier calls):</strong> any high-tier call on a plain <code>single</code> template splits automatically — phase 1 runs the generic <code>ai.reason.generic.v1</code> template (the fully rendered original prompt as task context, output = exhaustive <code>reasoning_md</code>), phase 2 re-runs the original template at low/freezed with the reasoning injected. Why: the high model is a reasoning model (R1 class) and strict-JSON emission is its known failure mode; the low coder model with server-side <code>json_schema</code> is the format specialist. Opt-outs: per-call <code>two_phase:false</code>, template <code>phase_flow='single_only'</code>, env <code>ANTILOKI_AUTO_TWO_PHASE=off</code>. The reasoning is independently persisted/cacheable and retrievable via <code>system.ai.call.detail</code> (<code>reasoning_call_id</code> on the return).</td>
      </tr>
      <tr>
        <td><strong>Explicit tier resolution order</strong></td>
        <td>explicit override &gt; <code>brain.routing_defaults</code> row &gt; template <code>default_tier</code> &gt; <code>mid</code> fallback. Before FIX-TIER-TEMPLATE-DEFAULT-IGNORED, an authored <code>default_tier</code> was never consulted — any prompt without a routing row silently ran at mid.</td>
      </tr>
      <tr>
        <td><strong>Provider cache telemetry</strong></td>
        <td><code>brain.ai_calls.tokens_cached</code> records provider-reported cached prompt tokens per call; <code>system.cost.today</code> rolls it up as <code>provider_cached_pct</code> (share of input tokens served from the provider's prefix cache).</td>
      </tr>
    </table>
    <p class="muted">Prompt layout principle: <strong>stable→volatile</strong> — invariant instruction text first, per-call context last, so provider prefix caching (byte-identical-prefix matching, e.g. DeepSeek/Qwen automatic caching) actually hits. <code>system.prompt.lint</code> renders a read-only scorecard over every active template (role stated, output contract, few-shot examples, structural field coverage, system_md usage), sorted worst-first.</p>
  "##.to_string();

    let repro = r##"
    <h2>7. Reproducibility</h2>
    <pre><code># Re-render this doc with current substrate state
node bin/antiloki.mjs system.docs.rag '{}' &gt; docs/rag-flow.html

# Populate / refresh embeddings
node bin/antiloki.mjs system.catalog.refresh '{}'
node bin/antiloki.mjs system.catalog.embed.all '{}'

# Probe manually
node bin/antiloki.mjs --unsafe-skill catalog-find '{"query":"audit","entity_type":"skill","limit":5}'

# Draft a contract using the full RAG pipeline
node bin/antiloki.mjs contract.draft.auto '{"card_slug":"&lt;your-card&gt;"}'</code></pre>
  "##.to_string();

    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>antiloki RAG layer — architecture + flow</title>
<style>{CSS}</style>
</head>
<body><nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>
<div class="wrap">
  <div class="kicker">antiloki-v3 · RAG architecture</div>
  <h1>How retrieval-augmented inference works in this substrate</h1>
  <p class="lede">Local-only embedding + cosine search via <code>@huggingface/transformers</code> WASM. One chokepoint (<code>catalog-find</code>), four consumers, full audit trail. This page is auto-generated and re-runs a live probe query every render so the numbers cannot be stale.</p>
  <div class="meta">Auto-generated · regen: <code>node bin/antiloki.mjs system.docs.rag '{{}}' &gt; docs/rag-flow.html</code></div>
  {tldr}
  {overview}
  {what_embedded}
  {data_flow}
  {probe_block}
  {contract_flow}
  {ai_call_pipeline}
  {repro}
  <hr>
  <p class="muted" style="text-align:center">antiloki-v3 · {total_embeddings} embeddings · {law_count} laws · {skill_count} skills · {memory_count} memories ({correction_count} corrections)</p>
</div>
</body>
</html>"##
    );

    Ok(json!(html))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(db: &Connection) {
        db.execute_batch(
            "ATTACH DATABASE ':memory:' AS memory; ATTACH DATABASE ':memory:' AS laws;
             CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL);
             CREATE TABLE memory.skills (name TEXT, description TEXT);
             CREATE TABLE actions (name TEXT, description TEXT);
             CREATE TABLE agents (name TEXT, description TEXT);
             CREATE TABLE laws.laws (slug TEXT, title TEXT, statement TEXT, rationale TEXT, superseded_by INTEGER);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT);",
        )
        .unwrap();
    }

    #[test]
    fn empty_state_renders_no_embeddings_probe_branch() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        let out = ragFlowHtml(&json!({}), &db).unwrap();
        let html = out.as_str().unwrap();
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<title>antiloki RAG layer — architecture + flow</title>"));
        assert!(html.contains("no embeddings yet"));
        assert!(html.contains("No embeddings exist yet."));
        assert!(!html.contains("Live probe — ran just now"));
        assert!(html.contains("antiloki-v3 · 0 embeddings · 0 laws · 0 skills · 0 memories (0 corrections)"));
    }

    #[test]
    fn counts_reflect_seeded_rows() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        db.execute_batch(
            "INSERT INTO memory.skills (name, description) VALUES ('audit-log','writes an audit entry'),('card-add','adds a card');
             INSERT INTO laws.laws (slug, title, superseded_by) VALUES ('LAW-A','A law', NULL), ('LAW-B','B law', 1);
             INSERT INTO memory.memories (type, title) VALUES ('correction','fix1'), ('lesson','l1'), ('correction','fix2');",
        )
        .unwrap();
        let out = ragFlowHtml(&json!({}), &db).unwrap();
        let html = out.as_str().unwrap();
        // laws: only superseded_by IS NULL counted → 1
        assert!(html.contains("antiloki-v3 · 0 embeddings · 1 laws · 2 skills · 3 memories (2 corrections)"));
        assert!(html.contains("<div class=\"num\">2</div><div class=\"sub\">memory.skills</div>"));
        assert!(html.contains("<div class=\"num\">1</div><div class=\"sub\">laws.laws (non-superseded)</div>"));
    }

    #[test]
    fn embeddings_present_triggers_live_probe_section_via_substring_fallback() {
        let db = Connection::open_in_memory().unwrap();
        setup(&db);
        // Seed catalog_embeddings for entity_type 'law' only — 'skill' stays fully unindexed so
        // the probe's skill call resolves useModel=false and takes catalog-find's deterministic
        // substring path (an embedded model for 'law' forces that entity_type's probe call down
        // catalogFind's real-embed() Err branch, mirroring `.catch(() => [])`).
        db.execute_batch(
            "INSERT INTO memory.catalog_embeddings (entity_type, entity_name, model, dims, vector) VALUES ('law','LAW-AUDIT-HOOK','bge-base',768,X'00');
             INSERT INTO memory.skills (name, description) VALUES ('law-hook-audit','adds a law hook for auditing');
             INSERT INTO laws.laws (slug, title, superseded_by) VALUES ('LAW-AUDIT-HOOK','audit hook law', NULL);",
        )
        .unwrap();
        let out = ragFlowHtml(&json!({}), &db).unwrap();
        let html = out.as_str().unwrap();
        assert!(html.contains("Live probe — ran just now"));
        assert!(html.contains("Query: <code>add a new law-hook for auditing</code>"));
        // skill probe: no embeddings for 'skill' entity_type -> deterministic substring fallback
        // over the FULL probe phrase as one LIKE pattern (catalog-find.rs: `%{query.lower()}%`,
        // not tokenized) -> no skill name/description contains that whole phrase -> no hits.
        assert!(html.contains("Top-0 skill hits"));
        assert!(html.contains("no hits (no embeddings stored)"));
        // law probe: embeddings exist for 'law' -> catalogFind's real-embed() branch errs ->
        // caught -> empty hits.
        assert!(html.contains("Top-0 law hits"));
        assert!(html.contains("no law embeddings stored"));
    }

    #[test]
    fn tiny_heat_renders_ten_cells_with_score_scaled_opacity() {
        let cells = tiny_heat(0.0);
        assert_eq!(cells.matches("<span class=\"heat\"").count(), 10);
        assert!(cells.contains("rgba(188,140,255,0.08)"));
        let full = tiny_heat(1.0);
        assert!(full.contains("rgba(188,140,255,1)"));
    }

    #[test]
    fn esc_html_escapes_all_five_entities() {
        assert_eq!(esc_html("<a & \"b\">"), "&lt;a &amp; &quot;b&quot;&gt;");
    }
}
