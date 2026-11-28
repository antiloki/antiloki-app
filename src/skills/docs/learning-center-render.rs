//! learning-center-render.rs — twin of skills/docs/learning-center-render.mjs (db lane, async
//! default export ignored per the playbook's async-over-sync rule — every await here is just a
//! sync SQLite read or a sync fs write). NAMES ARE CONTRACT: `learningCenterRender` (default).
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier entry for this file, per the crossing
//! playbook): the JS writes ten real HTML files under `out_dir`, which DEFAULTS to roots.mjs's
//! `LEARNING_CENTER_DIR = join(DATA_DIR, 'learning-center')` — a hardcoded-constant fs root
//! (env-var-overridable, never DB-derived), same category as god-driver-halt.rs's HALT_FILE /
//! artefact-store.rs's ARTEFACTS_DIR / laws-render-all.rs's PROJECT_ROOT. Real file writes, not
//! simulated; proven below via native `#[cfg(test)]` tests using a temp dir (either passed
//! explicitly via `input.out_dir` — this skill, unlike most fs-anchored siblings, exposes that
//! as a genuine input field — or injected via the `ANTILOKI_DATA_DIR` env seam for the default
//! path, mirroring artefact-store.rs's `with_data_dir` convention).
//!
//! `learning_center_dir()`: roots.mjs `LEARNING_CENTER_DIR` = `join(DATA_DIR, 'learning-center')`;
//! `DATA_DIR` = `ANTILOKI_DATA_DIR` env else `SUBSTRATE_ROOT/.antiloki`; `SUBSTRATE_ROOT` =
//! `ANTILOKI_SUBSTRATE_ROOT` env else self-derived (antiloki-core-rust's parent dir on this
//! checkout layout — same convention as every sibling roots.mjs-anchored twin).
//!
//! Cross-module reuse (per the playbook's cross-module rule, both twins already converted):
//! JS `section(...)` -> `crate::docs_learning_center_section::learningCenterSection`; JS
//! `excalidraw(...)` -> `crate::docs_excalidraw_embed::excalidrawEmbed`. Both ignore their
//! `context`/`db` argument internally (pure formatters) but keep the standard-lane
//! `(&Value, &Connection) -> Result<Value, String>` signature, so this twin just forwards its
//! own `db` reference through — harmless since neither callee reads it.
//!
//! `fetchCounts`'s `safe(sql)` (try/catch -> null -> `?? 0`) is ported as `safe_count`, matching
//! the `one`/`q` try-degrade convention already established in stats-overview.rs. Grouping
//! (`new Map()` insertion order for renderActions' namespace groups and renderLaws' domain
//! groups) uses the `order: Vec<..>` + `HashMap` pattern already established in laws-md.rs.
//!
//! `esc(s)` = `String(s ?? '').replace(/[&<>"']/g, ...)` — identical escaping to the sibling
//! learning-center-section.rs's private `escape_html_str`, reimplemented here per-file (no
//! shared helper module exists for it, same duplication precedent used throughout the crate).
//!
//! `JSON.parse(r.steps_json)` in renderWorkflows: JS only try/catches the `JSON.parse` call
//! itself — a malformed-JSON parse FAILURE is caught and `steps` stays `[]` (its initial value),
//! but a parse that SUCCEEDS with a non-array result (an object/number/string/bool, or `null`
//! from a SQL-NULL column — `JSON.parse(null)` coerces to the string `"null"` and parses fine)
//! leaves the subsequent unguarded `steps.map(...)` to throw uncaught, crashing the whole call.
//! Ported 1:1: malformed JSON -> empty Vec (caught); parse-succeeds-but-non-array -> `Err`
//! (uncaught crash, propagated as `Result::Err`); parse-succeeds-as-array -> normal render.
//!
//! `context.db` missing -> JS throws `err.contextMissing('learning-center-render', 'db')`. Not
//! modeled: Rust's `db: &Connection` is a required, non-nullable reference — same convention
//! used by every sibling twin.
//!
//! `html.length` (JS string length = UTF-16 code units, NOT byte length) ->
//! `.encode_utf16().count()`, same convention as laws-render-all.rs's `bytes` field.
// QUALITY: 78/100 — ten mechanical HTML renderers + fetchCounts, ported 1:1 from the JS source;
// real fs writes proven via native tests, not conform-db (bespoke-native, no fixture/frontier).

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

const MAX_AI_CALLS: i64 = 10;
const ALL_SLUGS: [&str; 10] = [
    "index", "overview", "agents", "actions", "workflows", "laws", "memory", "driver",
    "audit-chain", "dbs",
];

// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS `${v}`
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

// String(s ?? '').replace(/[&<>"']/g, ...) — single pass over a plain Rust &str.
fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}
// esc(v) = String(v ?? '').replace(...) — the JS module's own `esc` helper.
fn esc(v: Option<&Value>) -> String {
    escape_str(&str_qq(v))
}

// roots.mjs: LEARNING_CENTER_DIR = join(DATA_DIR, 'learning-center'). See module doc.
fn learning_center_dir() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_DATA_DIR") {
        if !v.is_empty() {
            return PathBuf::from(v).join("learning-center");
        }
    }
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v).join(".antiloki").join("learning-center");
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .join(".antiloki")
        .join("learning-center")
}

// Node's `path.resolve(out_dir, filename)`: joins the segments, lexically normalizes `.`/`..`
// components, and — if the result is still relative — absolutizes it against the current
// working directory. `PathBuf::join()` alone does neither of the latter two, which diverges from
// JS for a relative `out_dir` (see module doc). Used at the one call site that mirrors JS's
// `resolve(out_dir, ...)` — the per-page write path (`pages[].path` in the returned JSON).
fn resolve_like_node(out_dir: &Path, filename: &str) -> PathBuf {
    let joined = out_dir.join(filename);
    let normalized = normalize_lexical(&joined);
    if normalized.is_absolute() {
        normalized
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        normalize_lexical(&cwd.join(normalized))
    }
}

// Purely textual `.`/`..` collapse (no filesystem access, no symlink resolution) — matching
// Node's path.normalize semantics, which never touch the filesystem either.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(comp),
            },
            c => out.push(c),
        }
    }
    let mut result = PathBuf::new();
    for c in out {
        result.push(c.as_os_str());
    }
    result
}

struct Counts {
    actions: i64,
    agents: i64,
    cards: i64,
    contracts: i64,
    workflows: i64,
    laws: i64,
    memories: i64,
    timeline: i64,
}

// `const safe = (sql, p=[]) => { try { return db.prepare(sql).get(...p); } catch { return null; } }`
// then `safe(sql)?.c ?? 0`.
fn safe_count(db: &Connection, sql: &str) -> i64 {
    match crate::db_json::query_rows(db, sql, &[]) {
        Ok(v) => v
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("c"))
            .and_then(|c| c.as_i64())
            .unwrap_or(0),
        Err(_) => 0,
    }
}

fn fetch_counts(db: &Connection) -> Counts {
    Counts {
        actions: safe_count(db, "SELECT COUNT(*) AS c FROM actions"),
        agents: safe_count(db, "SELECT COUNT(*) AS c FROM agents"),
        cards: safe_count(db, "SELECT COUNT(*) AS c FROM cards"),
        contracts: safe_count(db, "SELECT COUNT(*) AS c FROM contracts"),
        workflows: safe_count(db, "SELECT COUNT(*) AS c FROM workflows"),
        laws: safe_count(db, "SELECT COUNT(*) AS c FROM laws.laws WHERE superseded_by IS NULL"),
        memories: safe_count(db, "SELECT COUNT(*) AS c FROM memory.memories WHERE COALESCE(superseded_by,'')=''"),
        timeline: safe_count(db, "SELECT COUNT(*) AS c FROM sacred_timeline"),
    }
}

fn render_section(db: &Connection, slug: &str, title: &str, body_html: String) -> Result<String, String> {
    let input = json!({ "slug": slug, "title": title, "body_html": body_html });
    let out = crate::docs_learning_center_section::learningCenterSection(&input, db)?;
    Ok(out.as_str().unwrap_or_default().to_string())
}

fn render_index(counts: &Counts, db: &Connection) -> Result<String, String> {
    let body = format!(
        "\n    <p>antiloki is a deterministic AI orchestration substrate. Five SQLite DBs, ten agents,\n       {actions} actions, {workflows} workflows.</p>\n    <h2>Start here</h2>\n    <ul>\n      <li><a href=\"./overview.html\">Overview</a> \u{2014} what antiloki is, in one screen</li>\n      <li><a href=\"./agents.html\">Agents</a> \u{2014} the {agents} personas + their grants</li>\n      <li><a href=\"./actions.html\">Actions</a> \u{2014} full catalog</li>\n      <li><a href=\"./workflows.html\">Workflows</a> \u{2014} declarative compositions</li>\n      <li><a href=\"./laws.html\">Laws</a> \u{2014} the {laws} runtime-enforced rules</li>\n      <li><a href=\"./memory.html\">Memory</a> \u{2014} tier-1/tier-2 + RAG flow</li>\n      <li><a href=\"./driver.html\">Driver</a> \u{2014} autonomous orchestration loop</li>\n      <li><a href=\"./audit-chain.html\">Audit chain</a> \u{2014} Merkle-anchored sacred_timeline</li>\n      <li><a href=\"./dbs.html\">5-DB layout</a></li>\n    </ul>\n    <h2>State at a glance</h2>\n    <table>\n      <tr><th>Concept</th><th>Count</th></tr>\n      <tr><td>actions</td><td>{actions}</td></tr>\n      <tr><td>agents</td><td>{agents}</td></tr>\n      <tr><td>workflows</td><td>{workflows}</td></tr>\n      <tr><td>active laws</td><td>{laws}</td></tr>\n      <tr><td>cards</td><td>{cards}</td></tr>\n      <tr><td>contracts</td><td>{contracts}</td></tr>\n      <tr><td>memories</td><td>{memories}</td></tr>\n      <tr><td>audit events</td><td>{timeline}</td></tr>\n    </table>\n  ",
        actions = counts.actions, workflows = counts.workflows, agents = counts.agents,
        laws = counts.laws, cards = counts.cards, contracts = counts.contracts,
        memories = counts.memories, timeline = counts.timeline,
    );
    render_section(db, "index", "antiloki learning center", body)
}

fn render_overview(db: &Connection) -> Result<String, String> {
    let counts = fetch_counts(db);
    let excalidraw_input = json!({
        "kind": "dbs",
        "nodes": [
            { "id": "state", "label": "state", "color": "#58a6ff" },
            { "id": "memory", "label": "memory", "color": "#3fb950" },
            { "id": "brain", "label": "brain", "color": "#d29922" },
            { "id": "laws", "label": "laws", "color": "#f85149" },
            { "id": "knowledge", "label": "knowledge", "color": "#8b949e" },
        ],
        "edges": [],
        "width": 700, "height": 280, "seed": 11,
    });
    let dbs_svg = crate::docs_excalidraw_embed::excalidrawEmbed(&excalidraw_input, db)?;
    let dbs_svg_str = dbs_svg.as_str().unwrap_or_default();
    let body = format!(
        "\n    <p>The substrate is event-sourced, audited, and git-mirrored. Every mutation flows through one\n       universal dispatcher (<code>action-run</code>) and lands in <code>sacred_timeline</code> with a\n       Merkle-chained digital print. The driver loop turns the cards board into an\n       orchestration target; agents act on cards via owned actions.</p>\n    <h2>The five DBs</h2>\n    {dbs_svg}\n    <ul>\n      <li><strong>state</strong> \u{2014} what IS now (cards, agents, actions, contracts, qa_runs)</li>\n      <li><strong>memory</strong> \u{2014} what was LEARNED (memories, embeddings, links)</li>\n      <li><strong>brain</strong> \u{2014} what THINKS (driver_ticks, ai_calls, decisions, locks)</li>\n      <li><strong>laws</strong> \u{2014} what MUST (constitutional + procedural rules + violations)</li>\n      <li><strong>knowledge</strong> \u{2014} what was COLLECTED (documents, extracts \u{2014} RAG corpus)</li>\n    </ul>\n    <h2>Counts right now</h2>\n    <p class=\"muted\">\n      {actions} actions \u{b7} {agents} agents \u{b7} {workflows} workflows \u{b7}\n      {laws} active laws \u{b7} {memories} memories \u{b7} {timeline} audit events\n    </p>\n  ",
        dbs_svg = dbs_svg_str, actions = counts.actions, agents = counts.agents, workflows = counts.workflows,
        laws = counts.laws, memories = counts.memories, timeline = counts.timeline,
    );
    render_section(db, "overview", "Overview", body)
}

fn render_agents(db: &Connection) -> Result<String, String> {
    let agents = crate::db_json::query_rows(db, "SELECT name, description FROM agents ORDER BY name", &[])?
        .as_array().cloned().unwrap_or_default();

    let mut rows = String::new();
    for a in &agents {
        rows.push_str(&format!(
            "\n    <tr><td><code>{}</code></td><td>{}</td></tr>\n  ",
            esc(a.get("name")), esc(a.get("description"))
        ));
    }

    let nodes: Vec<Value> = agents.iter().map(|a| {
        let name = a.get("name").cloned().unwrap_or(Value::Null);
        json!({ "id": name.clone(), "label": name })
    }).collect();
    let excalidraw_input = json!({ "kind": "agent-grants", "nodes": nodes, "edges": [], "width": 800, "height": 400, "seed": 7 });
    let grants_svg = crate::docs_excalidraw_embed::excalidrawEmbed(&excalidraw_input, db)?;
    let grants_svg_str = grants_svg.as_str().unwrap_or_default();

    let body = format!(
        "\n    <h2>Personas</h2>\n    <table><tr><th>name</th><th>role</th></tr>{rows}</table>\n    <h2>Grant graph</h2>\n    <p class=\"muted\">(node-only visualization; full grants live in <code>agent_actions</code>.)</p>\n    {grants_svg}\n  ",
        rows = rows, grants_svg = grants_svg_str
    );
    render_section(db, "agents", "Agents", body)
}

fn render_actions(db: &Connection) -> Result<String, String> {
    let rows_arr = crate::db_json::query_rows(db, "SELECT name, owner_agent, description FROM actions ORDER BY name", &[])?
        .as_array().cloned().unwrap_or_default();

    // `new Map()` groups by namespace prefix, in first-seen (== ORDER BY name) insertion order.
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<&Value>> = std::collections::HashMap::new();
    for r in &rows_arr {
        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let first_seg = name.split('.').next().unwrap_or("");
        let ns = format!("{first_seg}.");
        if !groups.contains_key(&ns) {
            order.push(ns.clone());
            groups.insert(ns.clone(), Vec::new());
        }
        groups.get_mut(&ns).unwrap().push(r);
    }

    let mut sections_html = String::new();
    for ns in &order {
        let items = &groups[ns];
        sections_html.push_str(&format!(
            "<h3>{}* <span class=\"badge\">{}</span></h3>",
            escape_str(ns), items.len()
        ));
        let mut table_rows = String::new();
        for i in items {
            table_rows.push_str(&format!(
                "<tr><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
                esc(i.get("name")), esc(i.get("owner_agent")), esc(i.get("description"))
            ));
        }
        sections_html.push_str(&format!(
            "<table><tr><th>name</th><th>owner</th><th>description</th></tr>{table_rows}</table>"
        ));
    }
    let body = format!("<p class=\"muted\">{} actions, grouped by namespace.</p>{}", rows_arr.len(), sections_html);
    render_section(db, "actions", "Actions", body)
}

fn render_workflows(db: &Connection) -> Result<String, String> {
    let rows_arr = match crate::db_json::query_rows(db, "SELECT name, owner_agent, description, steps_json FROM workflows ORDER BY name", &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    if rows_arr.is_empty() {
        return render_section(db, "workflows", "Workflows", "<p class=\"muted\">No workflows registered yet.</p>".to_string());
    }

    let mut blocks = String::new();
    for r in &rows_arr {
        // JS: `try { steps = JSON.parse(r.steps_json) } catch {}` guards ONLY the parse; the
        // later `steps.map(...)` is unguarded. See module doc for the exact fidelity split.
        let steps: Vec<Value> = match r.get("steps_json").and_then(|v| v.as_str()) {
            Some(text) => match serde_json::from_str::<Value>(text) {
                Ok(Value::Array(a)) => a,
                // Parse succeeded but yielded a non-array -> `steps.map` throws uncaught in JS.
                Ok(_) => {
                    return Err(
                        "learningCenterRender: workflows.steps_json parsed to a non-array value (steps.map is not a function)".to_string(),
                    )
                }
                // Parse itself threw (malformed JSON) -> caught -> steps keeps its `[]` initial value.
                Err(_) => Vec::new(),
            },
            // NULL steps_json column: `JSON.parse(null)` coerces to the string "null" and parses
            // successfully to JS `null` — not caught, and `null.map` throws uncaught in JS.
            None => {
                return Err(
                    "learningCenterRender: workflows.steps_json is null (JSON.parse(null) yields null; steps.map is not a function)".to_string(),
                )
            }
        };
        let mut step_rows = String::new();
        for (i, s) in steps.iter().enumerate() {
            step_rows.push_str(&format!(
                "\n      <tr><td>{}</td><td><code>{}</code></td><td>{}</td></tr>\n    ",
                i + 1, esc(s.get("action")), esc(s.get("capture_as"))
            ));
        }
        blocks.push_str(&format!(
            "\n      <h3>{} <span class=\"badge\">owner: {}</span></h3>\n      <p>{}</p>\n      <table><tr><th>#</th><th>action</th><th>capture_as</th></tr>{}</table>\n    ",
            esc(r.get("name")), esc(r.get("owner_agent")), esc(r.get("description")), step_rows
        ));
    }
    let body = format!(
        "<p class=\"muted\">{} workflows. Workflows compose actions; each step writes its own audit row.</p>{}",
        rows_arr.len(), blocks
    );
    render_section(db, "workflows", "Workflows", body)
}

fn render_laws(db: &Connection) -> Result<String, String> {
    let rows_arr = match crate::db_json::query_rows(db, "SELECT slug, kind, domain, title, enforcement FROM laws.laws WHERE superseded_by IS NULL ORDER BY domain, slug", &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // `new Map()` keyed by r.domain (possibly SQL NULL == JS null) — Option<String> preserves the
    // null-vs-empty-string distinction a plain "".to_string() coercion would collapse.
    let mut order: Vec<Option<String>> = Vec::new();
    let mut by_domain: std::collections::HashMap<Option<String>, Vec<&Value>> = std::collections::HashMap::new();
    for r in &rows_arr {
        let domain_key: Option<String> = match r.get("domain") {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        if !by_domain.contains_key(&domain_key) {
            order.push(domain_key.clone());
            by_domain.insert(domain_key.clone(), Vec::new());
        }
        by_domain.get_mut(&domain_key).unwrap().push(r);
    }

    let mut blocks = String::new();
    for domain in &order {
        let items = &by_domain[domain];
        let domain_value = domain.clone().map(Value::String).unwrap_or(Value::Null);
        let mut rows_html = String::new();
        for i in items {
            rows_html.push_str(&format!(
                "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(i.get("slug")), esc(i.get("kind")), esc(i.get("enforcement")), esc(i.get("title"))
            ));
        }
        blocks.push_str(&format!(
            "\n    <h3>{} <span class=\"badge\">{}</span></h3>\n    <table><tr><th>slug</th><th>kind</th><th>enforcement</th><th>title</th></tr>{}</table>\n  ",
            esc(Some(&domain_value)), items.len(), rows_html
        ));
    }
    let body = format!("<p class=\"muted\">{} active laws across {} domains.</p>{}", rows_arr.len(), order.len(), blocks);
    render_section(db, "laws", "Laws", body)
}

fn render_memory(db: &Connection) -> Result<String, String> {
    let rows_arr = match crate::db_json::query_rows(db, "SELECT type, COUNT(*) AS n FROM memory.memories WHERE COALESCE(superseded_by,'') = '' GROUP BY type ORDER BY type", &[]) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let excalidraw_input = json!({
        "kind": "memory-flow",
        "nodes": [
            { "id": "add",  "label": "memory.add", "color": "#3fb950" },
            { "id": "t1",   "label": "tier-1", "color": "#58a6ff" },
            { "id": "cons", "label": "consolidate", "color": "#d29922" },
            { "id": "t2",   "label": "tier-2 narratives", "color": "#58a6ff" },
            { "id": "rag",  "label": "RAG retrieval", "color": "#f85149" },
        ],
        "edges": [
            { "from": "add", "to": "t1", "label": "persist" },
            { "from": "t1", "to": "cons", "label": "pattern" },
            { "from": "cons", "to": "t2", "label": "distill" },
            { "from": "t2", "to": "rag", "label": "retrieve" },
            { "from": "t1", "to": "rag", "label": "retrieve" },
        ],
        "width": 800, "height": 320, "seed": 23,
    });
    let rag_svg = crate::docs_excalidraw_embed::excalidrawEmbed(&excalidraw_input, db)?;
    let rag_svg_str = rag_svg.as_str().unwrap_or_default();

    let mut rows_html = String::new();
    for r in &rows_arr {
        rows_html.push_str(&format!("<tr><td>{}</td><td>{}</td></tr>", esc(r.get("type")), tmpl(r.get("n"))));
    }

    let body = format!(
        "\n    <h2>Tiers</h2>\n    <ul>\n      <li><strong>tier-1</strong> \u{2014} raw memories (lesson/preference/decision/reference/correction)</li>\n      <li><strong>tier-2</strong> \u{2014} consolidated narratives, derived from tier-1 via <code>derived_from</code> links</li>\n    </ul>\n    <h2>Counts by type</h2>\n    <table><tr><th>type</th><th>count</th></tr>{rows_html}</table>\n    <h2>RAG flow</h2>\n    {rag_svg}\n    <h2>Auto-consolidation (FEAT-SESSION-END-CONSOLIDATION)</h2>\n    <p>On session end, deterministic pattern matchers extract candidate memories\n       (REPEATED_TOPIC, PREFERENCE_SIGNAL, REPEATED_CANCEL, SUCCESSFUL_REPEAT).\n       Confidence \u{2265} 0.7 \u{2192} auto-commit via keeper. Tagged\n       <code>auto-extracted</code> for traceability.</p>\n  ",
        rows_html = rows_html, rag_svg = rag_svg_str
    );
    render_section(db, "memory", "Memory layer", body)
}

fn render_driver(db: &Connection) -> Result<String, String> {
    let excalidraw_input = json!({
        "kind": "state-machine",
        "nodes": [
            { "id": "backlog", "label": "backlog" },
            { "id": "pl",      "label": "pl" },
            { "id": "tl",      "label": "tl" },
            { "id": "impl",    "label": "impl" },
            { "id": "qa",      "label": "qa" },
            { "id": "done",    "label": "done", "color": "#3fb950" },
        ],
        "edges": [
            { "from": "backlog", "to": "pl",   "label": "session.start" },
            { "from": "pl",      "to": "tl",   "label": "card ratified" },
            { "from": "tl",      "to": "impl", "label": "contract.sign" },
            { "from": "impl",    "to": "qa",   "label": "dev.implement" },
            { "from": "qa",      "to": "done", "label": "qa.pass" },
            { "from": "qa",      "to": "impl", "label": "qa.fail (retry)" },
        ],
        "width": 760, "height": 340, "seed": 17,
    });
    let svg = crate::docs_excalidraw_embed::excalidrawEmbed(&excalidraw_input, db)?;
    let svg_str = svg.as_str().unwrap_or_default();
    let body = format!(
        "\n    <h2>Tick lifecycle</h2>\n    <ol>\n      <li><code>brain.driver_ticks</code> row inserted (outcome=running)</li>\n      <li>halt-file check \u{2192} if present, outcome=halt_requested + return</li>\n      <li><code>driver-schedule</code> picks the next actionable card</li>\n      <li><code>driver-state-machine</code> rule-table \u{2192} next agent + action</li>\n      <li><code>action-run</code> with actor='driver:auto'</li>\n      <li>capture fired_event_id + cost_usd; finalize the tick row</li>\n    </ol>\n    <h2>Card flow</h2>\n    {svg}\n  ",
        svg = svg_str
    );
    render_section(db, "driver", "Driver loop", body)
}

fn render_audit_chain(db: &Connection) -> Result<String, String> {
    let body = "\n    <p>Every mutation lands in <code>sacred_timeline</code>:</p>\n    <pre><code>(id, ts, actor, operation, entity_type, entity_id, payload_json,\n prev_print, digital_print)</code></pre>\n    <p><code>digital_print</code> = SHA-256 of (<code>prev_print</code> + canonical event payload). Chain integrity\n       is cryptographic; any after-the-fact mutation breaks the next link.</p>\n    <h2>Replay model</h2>\n    <p>Given the chain, every state can be reconstructed: read events in id-order,\n       apply each as a deterministic transition. The git history mirrors this (one\n       commit per card per event) for human-readable replay.</p>\n    <h2>Triggers</h2>\n    <p>SQLite triggers BLOCK <code>UPDATE</code> + <code>DELETE</code> on sacred_timeline. Direct mutation is\n       impossible from any caller \u{2014} including the substrate's own code.</p>\n  ".to_string();
    render_section(db, "audit-chain", "Audit chain", body)
}

fn render_dbs(db: &Connection) -> Result<String, String> {
    let body = "\n    <h2>state.sqlite \u{2014} what IS</h2>\n    <p>Cards, actions, agents, contracts, qa_runs, sacred_timeline,\n       interviewer_sessions/turns, pl_*, workflows.</p>\n    <h2>memory.sqlite \u{2014} what LEARNED</h2>\n    <p>Memories (tier-1 + tier-2 narratives), embeddings, memory_links (GraphRAG edges),\n       memory_uses (self-learning), anatomy_snapshots.</p>\n    <h2>brain.sqlite \u{2014} what THINKS</h2>\n    <p>driver_ticks, ai_calls (cache), ai_providers, prompt_templates,\n       brain_fires, brain_decisions, brain_locks.</p>\n    <h2>laws.sqlite \u{2014} what MUST</h2>\n    <p>laws, law_clauses, law_links, law_violations.</p>\n    <h2>knowledge.sqlite \u{2014} what COLLECTED</h2>\n    <p>RAG corpus: documents + extracts.</p>\n  ".to_string();
    render_section(db, "dbs", "5-DB layout", body)
}

fn dispatch(slug: &str, counts: &Counts, db: &Connection) -> Result<String, String> {
    match slug {
        "index" => render_index(counts, db),
        "overview" => render_overview(db),
        "agents" => render_agents(db),
        "actions" => render_actions(db),
        "workflows" => render_workflows(db),
        "laws" => render_laws(db),
        "memory" => render_memory(db),
        "driver" => render_driver(db),
        "audit-chain" => render_audit_chain(db),
        "dbs" => render_dbs(db),
        other => unreachable!("dispatch called with slug {other:?} not in ALL_SLUGS"),
    }
}

pub fn learningCenterRender(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();

    // `out_dir = OUT_DIR` (destructuring default fires only on absent/undefined, NOT on a
    // present-and-null value): `{ out_dir: null }` skips the default, leaving out_dir === null,
    // and JS's `mkdirSync(null, ...)` throws `TypeError: The "path" argument must be of type
    // string or an instance of Buffer or URL. Received null`. So: key absent -> default; key
    // present as a string -> that string; key present as anything else (incl. null) -> propagate
    // an Err rather than silently falling back to the default dir. The exact string (default's
    // rendered path, or the caller's own) is echoed back untouched as `output_dir`; only the
    // per-page write path goes through resolve_like_node (Node's path.resolve semantics).
    let out_dir_display: String = match obj.and_then(|o| o.get("out_dir")) {
        None => learning_center_dir().to_string_lossy().into_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err("learningCenterRender: out_dir must be a string".to_string()),
    };
    let out_dir_path = PathBuf::from(&out_dir_display);

    if !out_dir_path.exists() {
        std::fs::create_dir_all(&out_dir_path).map_err(|e| e.to_string())?;
    }

    let counts = fetch_counts(db);

    // `pages = null` default, then `pages ?? Object.keys(RENDERERS)` — an explicit array
    // (including []) always wins; absent/null falls back to every slug in RENDERERS order.
    let target_pages: Vec<String> = match obj.and_then(|o| o.get("pages")).filter(|v| !v.is_null()) {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect(),
        _ => ALL_SLUGS.iter().map(|s| s.to_string()).collect(),
    };

    let mut written: Vec<Value> = Vec::new();
    for slug in &target_pages {
        if !ALL_SLUGS.contains(&slug.as_str()) {
            continue; // `RENDERERS[slug]` undefined -> `if (!fn) continue`
        }
        let html = dispatch(slug, &counts, db)?;
        let path = resolve_like_node(&out_dir_path, &format!("{slug}.html"));
        std::fs::write(&path, &html).map_err(|e| e.to_string())?;
        written.push(json!({
            "slug": slug,
            "path": path.to_string_lossy().into_owned(),
            "bytes": html.encode_utf16().count() as i64,
        }));
    }

    Ok(json!({
        "pages_written": written.len() as i64,
        "pages": written,
        "ai_calls_used": 0,
        "ai_budget": MAX_AI_CALLS,
        "output_dir": out_dir_display,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS laws; ATTACH ':memory:' AS memory; \
             CREATE TABLE agents (name TEXT, description TEXT); \
             CREATE TABLE actions (name TEXT, owner_agent TEXT, description TEXT); \
             CREATE TABLE cards (id INTEGER PRIMARY KEY); \
             CREATE TABLE contracts (id INTEGER PRIMARY KEY); \
             CREATE TABLE workflows (name TEXT, owner_agent TEXT, description TEXT, steps_json TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY); \
             CREATE TABLE laws.laws (domain TEXT, slug TEXT, kind TEXT, title TEXT, enforcement TEXT, superseded_by INTEGER); \
             CREATE TABLE memory.memories (type TEXT, superseded_by INTEGER);",
        )
        .unwrap();
        c
    }

    fn seed(c: &Connection) {
        c.execute_batch(
            "INSERT INTO agents (name, description) VALUES ('dev','Implements features'), ('qa', NULL); \
             INSERT INTO actions (name, owner_agent, description) VALUES \
               ('dev.card.add','dev','Add a card'), ('dev.card.update','dev','Update a card'), ('qa.run.start','qa','Start QA'); \
             INSERT INTO cards (id) VALUES (1),(2),(3); \
             INSERT INTO contracts (id) VALUES (1); \
             INSERT INTO workflows (name, owner_agent, description, steps_json) VALUES \
               ('onboard','dev','Onboarding flow','[{\"action\":\"dev.card.add\",\"capture_as\":\"card\"},{\"action\":\"qa.run.start\"}]'); \
             INSERT INTO sacred_timeline (id) VALUES (1),(2); \
             INSERT INTO laws.laws (domain, slug, kind, title, enforcement, superseded_by) VALUES \
               ('audit','NEVER_BYPASS_WITH_AUDIT','constitutional','All mutations via with-audit','hard',NULL), \
               ('dev','SURGICAL_EDITS','principle','Every changed line must trace','soft',NULL); \
             INSERT INTO memory.memories (type, superseded_by) VALUES ('lesson',NULL), ('lesson',NULL), ('decision',NULL);",
        )
        .unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-learning-center-render-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn renders_all_ten_pages_with_counts_and_writes_real_files() {
        let c = db();
        seed(&c);
        let dir = temp_dir("all-pages");
        let r = learningCenterRender(&json!({ "out_dir": dir.to_str().unwrap() }), &c).unwrap();

        assert_eq!(r["pages_written"], json!(10));
        assert_eq!(r["ai_calls_used"], json!(0));
        assert_eq!(r["ai_budget"], json!(10));
        assert_eq!(r["output_dir"], json!(dir.to_str().unwrap()));

        let pages = r["pages"].as_array().unwrap();
        let slugs: Vec<&str> = pages.iter().map(|p| p["slug"].as_str().unwrap()).collect();
        assert_eq!(
            slugs,
            vec!["index", "overview", "agents", "actions", "workflows", "laws", "memory", "driver", "audit-chain", "dbs"]
        );
        for slug in &slugs {
            assert!(dir.join(format!("{slug}.html")).exists(), "missing {slug}.html");
        }

        // counts: actions=3, agents=2, cards=3, contracts=1, workflows=1, laws=2, memories=3, timeline=2
        let index_html = fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(index_html.contains("3 actions, 1 workflows"));
        assert!(index_html.contains("<td>agents</td><td>2</td>"));
        assert!(index_html.contains("<td>active laws</td><td>2</td>"));
        assert!(index_html.contains("<td>cards</td><td>3</td>"));
        assert!(index_html.contains("<td>memories</td><td>3</td>"));
        assert!(index_html.contains("<td>audit events</td><td>2</td>"));

        let overview_html = fs::read_to_string(dir.join("overview.html")).unwrap();
        assert!(overview_html.contains("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(overview_html.contains("[dbs]"));

        let agents_html = fs::read_to_string(dir.join("agents.html")).unwrap();
        assert!(agents_html.contains("<code>dev</code></td><td>Implements features</td>"));
        assert!(agents_html.contains("<code>qa</code></td><td></td>")); // null description -> ''
        assert!(agents_html.contains("[agent-grants]"));

        let actions_html = fs::read_to_string(dir.join("actions.html")).unwrap();
        assert!(actions_html.contains("<h3>dev.* <span class=\"badge\">2</span></h3>"));
        assert!(actions_html.contains("<h3>qa.* <span class=\"badge\">1</span></h3>"));
        assert!(actions_html.contains("<code>dev.card.add</code>"));

        let workflows_html = fs::read_to_string(dir.join("workflows.html")).unwrap();
        assert!(workflows_html.contains("1 workflows. Workflows compose actions"));
        assert!(workflows_html.contains("<code>dev.card.add</code></td><td>card</td>"));
        assert!(workflows_html.contains("<code>qa.run.start</code></td><td></td>")); // capture_as absent -> ''

        let laws_html = fs::read_to_string(dir.join("laws.html")).unwrap();
        assert!(laws_html.contains("2 active laws across 2 domains"));
        let audit_idx = laws_html.find("<h3>audit").unwrap();
        let dev_idx = laws_html.find("<h3>dev").unwrap();
        assert!(audit_idx < dev_idx); // ORDER BY domain ASC: audit before dev

        let memory_html = fs::read_to_string(dir.join("memory.html")).unwrap();
        assert!(memory_html.contains("<tr><td>decision</td><td>1</td></tr>")); // ORDER BY type: decision before lesson
        assert!(memory_html.contains("<tr><td>lesson</td><td>2</td></tr>"));
        assert!(memory_html.contains("[memory-flow]"));

        let driver_html = fs::read_to_string(dir.join("driver.html")).unwrap();
        assert!(driver_html.contains("[state-machine]"));

        let audit_chain_html = fs::read_to_string(dir.join("audit-chain.html")).unwrap();
        assert!(audit_chain_html.contains("sacred_timeline"));

        let dbs_html = fs::read_to_string(dir.join("dbs.html")).unwrap();
        assert!(dbs_html.contains("state.sqlite"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn partial_pages_list_skips_unknown_slugs() {
        let c = db();
        seed(&c);
        let dir = temp_dir("partial");
        let r = learningCenterRender(
            &json!({ "out_dir": dir.to_str().unwrap(), "pages": ["agents", "bogus", "dbs"] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["pages_written"], json!(2));
        let slugs: Vec<&str> = r["pages"].as_array().unwrap().iter().map(|p| p["slug"].as_str().unwrap()).collect();
        assert_eq!(slugs, vec!["agents", "dbs"]);
        assert!(dir.join("agents.html").exists());
        assert!(dir.join("dbs.html").exists());
        assert!(!dir.join("index.html").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_pages_array_renders_nothing_but_still_creates_dir() {
        let c = db();
        seed(&c);
        let dir = temp_dir("empty-pages");
        assert!(!dir.exists());
        let r = learningCenterRender(&json!({ "out_dir": dir.to_str().unwrap(), "pages": [] }), &c).unwrap();
        assert_eq!(r["pages_written"], json!(0));
        assert_eq!(r["pages"], json!([]));
        assert!(dir.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn workflows_page_reports_none_registered_when_table_empty() {
        let c = db(); // unseeded: workflows table empty
        let dir = temp_dir("no-workflows");
        let r = learningCenterRender(&json!({ "out_dir": dir.to_str().unwrap(), "pages": ["workflows"] }), &c).unwrap();
        assert_eq!(r["pages_written"], json!(1));
        let html = fs::read_to_string(dir.join("workflows.html")).unwrap();
        assert!(html.contains("No workflows registered yet."));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reported_bytes_match_utf16_code_unit_length_of_written_content() {
        let c = db();
        seed(&c);
        let dir = temp_dir("bytes-utf16");
        let r = learningCenterRender(&json!({ "out_dir": dir.to_str().unwrap(), "pages": ["dbs"] }), &c).unwrap();
        let content = fs::read_to_string(dir.join("dbs.html")).unwrap();
        let expected = content.encode_utf16().count() as i64;
        assert_eq!(r["pages"][0]["bytes"], json!(expected));
        // dbs.html's body text contains several em dashes (3 UTF-8 bytes, 1 UTF-16 code unit each)
        assert_ne!(content.len() as i64, expected);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn learning_center_dir_prefers_data_dir_env_then_substrate_root_then_manifest_parent() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("ANTILOKI_DATA_DIR");
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        let expected = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join(".antiloki").join("learning-center");
        assert_eq!(learning_center_dir(), expected);

        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", "C:/somewhere/sub");
        }
        assert_eq!(learning_center_dir(), PathBuf::from("C:/somewhere/sub").join(".antiloki").join("learning-center"));
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }

        unsafe {
            std::env::set_var("ANTILOKI_DATA_DIR", "C:/somewhere/data");
        }
        assert_eq!(learning_center_dir(), PathBuf::from("C:/somewhere/data").join("learning-center"));
        unsafe {
            std::env::remove_var("ANTILOKI_DATA_DIR");
        }
    }

    #[test]
    fn default_out_dir_resolves_via_antiloki_data_dir_env_override() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let dir = temp_dir("default-out-dir");
        unsafe {
            std::env::set_var("ANTILOKI_DATA_DIR", dir.to_str().unwrap());
        }
        let c = db();
        seed(&c);
        let result = learningCenterRender(&json!({ "pages": ["dbs"] }), &c);
        unsafe {
            std::env::remove_var("ANTILOKI_DATA_DIR");
        }
        let r = result.unwrap();
        let expected_dir = dir.join("learning-center");
        assert_eq!(r["output_dir"], json!(expected_dir.to_string_lossy().into_owned()));
        assert!(expected_dir.join("dbs.html").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    // --- Bug 1: out_dir null-vs-absent (JS destructuring default fires only on undefined) ---

    #[test]
    fn out_dir_present_and_null_errors_instead_of_silently_using_default() {
        let c = db();
        seed(&c);
        // `{ out_dir: null }` in JS skips the `= OUT_DIR` default (it only fires on absent/
        // undefined), leaving out_dir === null, and `mkdirSync(null, ...)` throws. Must NOT
        // silently fall back to the default learning-center dir.
        let result = learningCenterRender(&json!({ "out_dir": null, "pages": ["dbs"] }), &c);
        assert!(result.is_err(), "out_dir: null must error, not silently use the default dir");
    }

    #[test]
    fn out_dir_absent_key_falls_back_to_default() {
        let c = db();
        seed(&c);
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let dir = temp_dir("out-dir-absent-default");
        unsafe {
            std::env::set_var("ANTILOKI_DATA_DIR", dir.to_str().unwrap());
        }
        // `pages` present but `out_dir` key entirely absent -> the `= OUT_DIR` default fires.
        let result = learningCenterRender(&json!({ "pages": ["dbs"] }), &c);
        unsafe {
            std::env::remove_var("ANTILOKI_DATA_DIR");
        }
        let r = result.unwrap();
        assert_eq!(r["output_dir"], json!(dir.join("learning-center").to_string_lossy().into_owned()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn out_dir_present_as_non_string_errors() {
        let c = db();
        seed(&c);
        let result = learningCenterRender(&json!({ "out_dir": 42, "pages": ["dbs"] }), &c);
        assert!(result.is_err(), "out_dir: 42 (non-string, non-null) must error, matching JS's non-string mkdirSync/path.resolve failure");
    }

    // --- Bug 2: path.resolve() semantics (normalize `.`/`..` + absolutize against cwd) ---

    #[test]
    fn resolve_like_node_normalizes_dot_dot_segments_and_absolutizes_relative_paths() {
        let cwd = std::env::current_dir().unwrap();

        // Relative out_dir -> absolutized against cwd, matching Node's path.resolve.
        let resolved = resolve_like_node(Path::new("some/relative/dir"), "dbs.html");
        assert_eq!(resolved, cwd.join("some").join("relative").join("dir").join("dbs.html"));

        // `.`/`..` segments collapse lexically, same as Node's path.resolve normalization.
        let resolved2 = resolve_like_node(Path::new("a/b/../c/./d"), "dbs.html");
        assert_eq!(resolved2, cwd.join("a").join("c").join("d").join("dbs.html"));

        // Already-absolute out_dir stays absolute (just joined + normalized) — no cwd prepended.
        let abs_dir = cwd.join("already").join("absolute");
        let resolved3 = resolve_like_node(&abs_dir, "dbs.html");
        assert_eq!(resolved3, abs_dir.join("dbs.html"));
    }

    #[test]
    fn relative_out_dir_produces_absolute_resolved_page_paths_end_to_end() {
        let c = db();
        seed(&c);
        let cwd = std::env::current_dir().unwrap();
        let rel = format!("antiloki-lc-render-rs-relative-test-{}", std::process::id());
        let abs_dir = cwd.join(&rel);
        let _ = fs::remove_dir_all(&abs_dir);

        let r = learningCenterRender(&json!({ "out_dir": rel, "pages": ["dbs"] }), &c).unwrap();
        // `output_dir` echoes the caller's raw (relative) string, matching JS's `output_dir: out_dir`.
        assert_eq!(r["output_dir"], json!(rel));
        // but the per-page `path` is resolved absolute, matching JS's `resolve(out_dir, ...)`.
        let path = r["pages"][0]["path"].as_str().unwrap();
        assert!(Path::new(path).is_absolute(), "resolved page path must be absolute: {path}");
        assert_eq!(PathBuf::from(path), abs_dir.join("dbs.html"));
        assert!(abs_dir.join("dbs.html").exists());

        let _ = fs::remove_dir_all(&abs_dir);
    }

    // --- Bug 3: JSON.parse-then-map crash propagation in renderWorkflows ---

    #[test]
    fn workflows_steps_json_null_column_propagates_error_matching_js_map_crash() {
        let c = db();
        c.execute_batch(
            "INSERT INTO workflows (name, owner_agent, description, steps_json) VALUES \
             ('broken', 'dev', 'null steps_json', NULL);",
        )
        .unwrap();
        let dir = temp_dir("workflows-null-steps");
        let result = learningCenterRender(&json!({ "out_dir": dir.to_str().unwrap(), "pages": ["workflows"] }), &c);
        assert!(
            result.is_err(),
            "NULL steps_json: JSON.parse(null) succeeds as null, then `.map` on null crashes uncaught in JS"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn workflows_steps_json_valid_but_non_array_propagates_error() {
        let c = db();
        c.execute_batch(
            "INSERT INTO workflows (name, owner_agent, description, steps_json) VALUES \
             ('broken', 'dev', 'object steps_json', '{}');",
        )
        .unwrap();
        let dir = temp_dir("workflows-object-steps");
        let result = learningCenterRender(&json!({ "out_dir": dir.to_str().unwrap(), "pages": ["workflows"] }), &c);
        assert!(
            result.is_err(),
            "steps_json parsing to {{}} (non-array): JSON.parse succeeds, then `.map` throws uncaught in JS"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn workflows_steps_json_malformed_json_still_degrades_silently_per_try_catch() {
        let c = db();
        c.execute_batch(
            "INSERT INTO workflows (name, owner_agent, description, steps_json) VALUES \
             ('broken', 'dev', 'malformed steps_json', '{not valid json');",
        )
        .unwrap();
        let dir = temp_dir("workflows-malformed-steps");
        let r = learningCenterRender(&json!({ "out_dir": dir.to_str().unwrap(), "pages": ["workflows"] }), &c).unwrap();
        assert_eq!(r["pages_written"], json!(1));
        let html = fs::read_to_string(dir.join("workflows.html")).unwrap();
        assert!(html.contains("broken"));
        let _ = fs::remove_dir_all(&dir);
    }
}
