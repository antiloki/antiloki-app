//! memory-graph-html.rs — twin of skills/render/memory-graph-html.mjs (db READ lane, cross-db
//! `memory`). NAMES ARE CONTRACT: `memoryGraphHtml` (default). Renders the memory graph as a
//! standalone HTML+SVG page: concentric rings from a seed memory (delegates the BFS to
//! entities_memory_graph::memoryGraph) or a grid of the whole active graph when no seed is given.
//! CONFIRMED BY LIVE EXECUTION (node --input-type=module against the real .mjs, node:sqlite): the
//! source file is CRLF on disk, but ECMAScript template literals normalize every embedded line
//! terminator to a single LF — so every multi-line HTML/SVG block below uses plain "\n", never
//! "\r\n". `.join('\n')` separators are likewise plain LF (an explicit string literal, unaffected
//! by file encoding either way). TRIG CAVEAT: layoutConcentric's ring placement calls cos/sin on
//! `2*PI*i/n - PI/2`; V8 ships its own portable fdlibm cos/sin while Rust links the platform libm
//! (MSVCRT/UCRT on Windows) — the two CAN disagree in the last bit for non-"nice" angles, which
//! would flip the tail digit of a shortest-round-trip decimal embedded in the SVG string. This is
//! a real cross-engine risk inherent to the source (not a transcription gap); grid-mode layout
//! uses only sqrt/ceil/integer arithmetic (IEEE-754-mandated, exact) and carries none of it.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }

// JS Number.prototype.toString() (radix 10) / `${x}` for a number — ECMA-262 Number::toString.
// Rust's f64 Display NEVER switches to exponential notation; JS does for very small/large
// magnitudes (normalized exponent < -6 or > 20) — this skill's trig-based node coordinates (e.g.
// cos(-PI/2) ~= 7.960204194457796e-15) hit that range directly. Rust's `{:e}` shortest-round-trip
// digit sequence already matches what JS would produce (same class of algorithm) — only the
// notation choice/threshold/punctuation (JS: no leading zero on exponent, explicit '+' for
// positive exponent) differs, so this rebuilds JS's exact layout around Rust's digits. Verified
// by hand against 16 node-runtime reference cases spanning both notation regimes and both signs.
fn num_str(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); } // JS: (-0).toString() === "0"
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs); // e.g. "7.960204194457796e-15" or "1e20"
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k {
                format!("{digits}{}", "0".repeat((exp + 1 - k) as usize))
            } else {
                let (int_part, frac_part) = digits.split_at((exp + 1) as usize);
                format!("{int_part}.{frac_part}")
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        let sign = if exp >= 0 { "+" } else { "-" };
        format!("{m}e{sign}{}", exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}

fn escape_html(v: &Value) -> String {
    js_string(v).replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn truncate(v: &Value, max: usize) -> String {
    let s = js_string(v);
    if s.chars().count() <= max { return s; }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('\u{2026}');
    t
}

fn type_color(t: &str) -> &'static str {
    match t {
        "lesson" => "#4caf50",
        "preference" => "#5b9bd5",
        "decision" => "#e4b948",
        "reference" => "#88ddff",
        "correction" => "#ff6b6b",
        _ => "#888",
    }
}
fn kind_color(k: &str) -> &'static str {
    match k {
        "contradicts" => "#ff6b6b",
        "extends" => "#4caf50",
        "derived_from" => "#e4b948",
        "related" => "#888888",
        "corrects" => "#ff8c42",
        _ => "#888",
    }
}

// Verbatim transcription of the JS template-literal string (CRLF-on-disk, LF-cooked — see module
// doc). Byte-for-byte from a live `console.log(JSON.stringify(CSS))` run against the real source.
const CSS: &str = "\n:root { --bg:#1e1e1e; --fg:#ddd; --muted:#888; --bar:#444; --card:#2a2a2a; }\n* { box-sizing: border-box; }\nbody { font-family: 'JetBrains Mono', Consolas, monospace; padding: 1.5em; background: var(--bg); color: var(--fg); max-width: 1400px; margin: 0 auto; line-height: 1.5; }\nh1 { color: #fff; font-weight: 400; margin: 0 0 0.5em 0; }\n.meta { color: var(--muted); font-size: 0.85em; margin-bottom: 1em; }\n.legend { display: flex; gap: 2em; margin: 0.5em 0 1em; padding: 0.7em 1em; background: var(--card); border-radius: 4px; flex-wrap: wrap; }\n.legend section { display: flex; flex-direction: column; gap: 0.3em; }\n.legend h3 { margin: 0; font-size: 0.85em; color: var(--muted); font-weight: 500; }\n.legend .swatch { display: inline-block; width: 14px; height: 14px; border-radius: 3px; margin-right: 0.4em; vertical-align: middle; }\n.legend .item { font-size: 0.85em; }\n.svg-wrap { background: var(--card); border-radius: 4px; padding: 1em; overflow: auto; }\nsvg { display: block; margin: 0 auto; }\n.node-label { font-size: 11px; fill: #fff; pointer-events: none; }\n.node-id { font-size: 9px; fill: var(--muted); pointer-events: none; }\n.empty { color: var(--muted); font-style: italic; padding: 2em; text-align: center; }\n.detail-list { margin: 1em 0; }\n.detail-list .item { background: var(--card); padding: 0.6em 1em; border-radius: 3px; margin: 0.3em 0; border-left: 3px solid; font-size: 0.85em; }\n.detail-list .item .title { color: #fff; }\n.detail-list .item .muted { color: var(--muted); }\n";

// renderLegend() is 100% static (TYPE_COLOR/KIND_COLOR are compile-time data) — precomputed once,
// verified against a live call to the real function.
const LEGEND_HTML: &str = "<div class=\"legend\">\n  <section><h3>node = memory.type</h3><span class=\"item\"><span class=\"swatch\" style=\"background:#4caf50\"></span>lesson</span><span class=\"item\"><span class=\"swatch\" style=\"background:#5b9bd5\"></span>preference</span><span class=\"item\"><span class=\"swatch\" style=\"background:#e4b948\"></span>decision</span><span class=\"item\"><span class=\"swatch\" style=\"background:#88ddff\"></span>reference</span><span class=\"item\"><span class=\"swatch\" style=\"background:#ff6b6b\"></span>correction</span></section>\n  <section><h3>edge = link.kind</h3><span class=\"item\"><span class=\"swatch\" style=\"background:#ff6b6b\"></span>contradicts</span><span class=\"item\"><span class=\"swatch\" style=\"background:#4caf50\"></span>extends</span><span class=\"item\"><span class=\"swatch\" style=\"background:#e4b948\"></span>derived_from</span><span class=\"item\"><span class=\"swatch\" style=\"background:#888888\"></span>related</span><span class=\"item\"><span class=\"swatch\" style=\"background:#ff8c42\"></span>corrects</span></section>\n</div>";

fn layout_concentric(nodes: &[Value], edges: &[Value], seed_id: i64) -> HashMap<i64, (f64, f64)> {
    // `depths` mirrors the JS Map's insertion order (seed first, then BFS-discovered ids in
    // strictly non-decreasing depth order, then orphans last) — BTreeMap<depth, Vec<id>> below
    // reproduces the same grouping because JS's own insertion order is already depth-ascending.
    let mut depths: Vec<(i64, i64)> = vec![(seed_id, 0)];
    let mut depth_of: HashMap<i64, i64> = HashMap::new();
    depth_of.insert(seed_id, 0);

    let mut adj: HashMap<i64, Vec<i64>> = HashMap::new();
    for e in edges {
        let from = e.get("from_memory_id").and_then(|v| v.as_i64()).unwrap_or_default();
        let to = e.get("to_memory_id").and_then(|v| v.as_i64()).unwrap_or_default();
        adj.entry(from).or_default().push(to);
        adj.entry(to).or_default().push(from);
    }

    let mut frontier: Vec<i64> = vec![seed_id];
    let mut d: i64 = 0;
    while !frontier.is_empty() {
        let mut next: Vec<i64> = Vec::new();
        for id in &frontier {
            if let Some(nbs) = adj.get(id) {
                for &nb in nbs {
                    if !depth_of.contains_key(&nb) {
                        depth_of.insert(nb, d + 1);
                        depths.push((nb, d + 1));
                        next.push(nb);
                    }
                }
            }
        }
        frontier = next;
        d += 1;
    }

    // Any orphan nodes (present in `nodes` but never reached by the edge-derived BFS) get maxDepth+1.
    let max_depth = depth_of.values().cloned().fold(0i64, i64::max);
    for n in nodes {
        if let Some(id) = n.get("id").and_then(|v| v.as_i64()) {
            if !depth_of.contains_key(&id) {
                depth_of.insert(id, max_depth + 1);
                depths.push((id, max_depth + 1));
            }
        }
    }

    let mut by_depth: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for (id, dep) in &depths {
        by_depth.entry(*dep).or_default().push(*id);
    }

    let mut positions: HashMap<i64, (f64, f64)> = HashMap::new();
    const RADIUS_STEP: f64 = 130.0;
    for (dep, ids) in &by_depth {
        if *dep == 0 {
            positions.insert(seed_id, (0.0, 0.0));
            continue;
        }
        let r = (*dep as f64) * RADIUS_STEP;
        let n = ids.len();
        for (i, id) in ids.iter().enumerate() {
            let angle = (2.0 * std::f64::consts::PI * (i as f64)) / (n as f64) - std::f64::consts::PI / 2.0;
            positions.insert(*id, (r * angle.cos(), r * angle.sin()));
        }
    }
    positions
}

fn layout_grid(nodes: &[Value]) -> HashMap<i64, (f64, f64)> {
    let mut positions = HashMap::new();
    let n = nodes.len();
    if n == 0 {
        return positions;
    }
    const SPACING: f64 = 150.0;
    let cols = (n as f64).sqrt().ceil();
    let cols_usize = cols as usize;
    let rows_total = ((n as f64) / cols).ceil();
    for (i, node) in nodes.iter().enumerate() {
        let col = (i % cols_usize) as f64;
        let row = (i / cols_usize) as f64;
        let x = (col - (cols - 1.0) / 2.0) * SPACING;
        let y = (row - (rows_total - 1.0) / 2.0) * SPACING;
        if let Some(id) = node.get("id").and_then(|v| v.as_i64()) {
            positions.insert(id, (x, y));
        }
    }
    positions
}

fn render_svg(positions: &HashMap<i64, (f64, f64)>, nodes: &[Value], edges: &[Value]) -> String {
    let xs: Vec<f64> = positions.values().map(|p| p.0).collect();
    let ys: Vec<f64> = positions.values().map(|p| p.1).collect();
    let min_x = xs.iter().cloned().fold(-50.0_f64, f64::min);
    let max_x = xs.iter().cloned().fold(50.0_f64, f64::max);
    let min_y = ys.iter().cloned().fold(-50.0_f64, f64::min);
    let max_y = ys.iter().cloned().fold(50.0_f64, f64::max);
    let pad = 100.0_f64;
    let vb_x = min_x - pad;
    let vb_y = min_y - pad;
    let vb_w = (max_x - min_x) + pad * 2.0;
    let vb_h = (max_y - min_y) + pad * 2.0;

    // Edges first (so they render under nodes). `positions` is keyed by the edge-derived BFS, so a
    // link whose target was excluded from `nodes` (superseded, concentric mode) still has a
    // position — the line renders with no matching <g> node, exactly like the JS.
    let edges_svg: Vec<String> = edges
        .iter()
        .filter_map(|e| {
            let from = e.get("from_memory_id").and_then(|v| v.as_i64()).and_then(|id| positions.get(&id));
            let to = e.get("to_memory_id").and_then(|v| v.as_i64()).and_then(|id| positions.get(&id));
            let (from, to) = match (from, to) {
                (Some(f), Some(t)) => (*f, *t),
                _ => return None,
            };
            let kind_v = e.get("kind").cloned().unwrap_or(Value::Null);
            let kind_str = kind_v.as_str().unwrap_or("").to_string();
            let color = kind_color(&kind_str);
            let dash = if kind_str == "contradicts" { "stroke-dasharray=\"4 3\"" } else { "" };
            let weight = e.get("weight").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let rationale_v = e.get("rationale").cloned().unwrap_or(Value::Null);
            let rationale_part = if truthy(Some(&rationale_v)) { format!("\u{b7} {}", escape_html(&rationale_v)) } else { String::new() };
            Some(format!(
                "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-width=\"{}\" opacity=\"0.7\" {}>\n  <title>{} {}</title>\n</line>",
                num_str(from.0), num_str(from.1), num_str(to.0), num_str(to.1), color, num_str(1.5 + weight), dash,
                escape_html(&kind_v), rationale_part
            ))
        })
        .collect();

    let nodes_svg: Vec<String> = nodes
        .iter()
        .filter_map(|n| {
            let id = n.get("id").and_then(|v| v.as_i64())?;
            let pos = positions.get(&id)?;
            let type_v = n.get("type").cloned().unwrap_or(Value::Null);
            let type_str = type_v.as_str().unwrap_or("").to_string();
            let color = type_color(&type_str);
            let title_v = n.get("title").cloned().unwrap_or(Value::Null);
            let label = truncate(&title_v, 28);
            let valid_badge = if truthy(n.get("validated")) { "\u{2713}" } else { "" };
            let situation_v = n.get("situation").cloned().unwrap_or(Value::Null);
            let guidance_v = n.get("guidance").cloned().unwrap_or(Value::Null);
            Some(format!(
                "<g transform=\"translate({},{})\">\n  <circle r=\"28\" fill=\"{}\" opacity=\"0.85\" stroke=\"#fff\" stroke-width=\"1.5\"/>\n  <text class=\"node-id\" text-anchor=\"middle\" y=\"-4\">#{} {}</text>\n  <text class=\"node-label\" text-anchor=\"middle\" y=\"10\">{}</text>\n  <title>[{}] {}\nwhen: {}\ndo:   {}</title>\n</g>",
                num_str(pos.0), num_str(pos.1), color, id, valid_badge, escape_html(&Value::String(label)),
                escape_html(&type_v), escape_html(&title_v), escape_html(&situation_v), escape_html(&guidance_v)
            ))
        })
        .collect();

    format!(
        "<svg viewBox=\"{} {} {} {}\" width=\"100%\" height=\"{}\" xmlns=\"http://www.w3.org/2000/svg\">\n{}\n{}\n</svg>",
        num_str(vb_x), num_str(vb_y), num_str(vb_w), num_str(vb_h), num_str(vb_h.min(800.0)),
        edges_svg.join("\n"), nodes_svg.join("\n")
    )
}

fn render_detail_list(nodes: &[Value]) -> String {
    if nodes.is_empty() {
        return String::new();
    }
    let items: Vec<String> = nodes
        .iter()
        .map(|n| {
            let id_v = n.get("id").cloned().unwrap_or(Value::Null);
            let type_v = n.get("type").cloned().unwrap_or(Value::Null);
            let type_str = type_v.as_str().unwrap_or("").to_string();
            let color = type_color(&type_str);
            let title_v = n.get("title").cloned().unwrap_or(Value::Null);
            let situation_v = n.get("situation").cloned().unwrap_or(Value::Null);
            let guidance_v = n.get("guidance").cloned().unwrap_or(Value::Null);
            format!(
                "<div class=\"item\" style=\"border-left-color:{}\">\n  <span class=\"muted\">#{} [{}]</span> <span class=\"title\">{}</span>\n  <br><span class=\"muted\">when:</span> {}\n  <br><span class=\"muted\">do:</span> {}\n</div>",
                color, tmpl(Some(&id_v)), escape_html(&type_v), escape_html(&title_v), escape_html(&situation_v), escape_html(&guidance_v)
            )
        })
        .collect();
    format!(
        "<h2 style=\"color:#fff;font-weight:400;border-bottom:1px solid var(--bar);padding-bottom:0.3em;\">Nodes ({})</h2>\n<div class=\"detail-list\">\n{}\n</div>",
        nodes.len(),
        items.join("\n")
    )
}

pub fn memoryGraphHtml(input: &Value, db: &Connection) -> Result<Value, String> {
    let seed_raw = input.get("seed_memory_id");
    let seed_present = seed_raw.map_or(false, |v| !v.is_null());
    // JS default params trigger only on `undefined` (absent key), NOT on an explicit `null`.
    let depth_v: Value = match input.get("depth") { None => json!(3), Some(v) => v.clone() };
    let kinds_v: Value = match input.get("kinds") { None => Value::Null, Some(v) => v.clone() };

    let seed_id_opt: Option<i64> = if seed_present {
        match seed_raw.and_then(|v| v.as_i64()) {
            Some(id) => Some(id),
            None => return Err("memory-graph-html: seed_memory_id must be an integer".to_string()),
        }
    } else {
        None
    };

    let (nodes, edges, layout_mode): (Vec<Value>, Vec<Value>, &str) = if let Some(seed_id) = seed_id_opt {
        let g = crate::entities_memory_graph::memoryGraph(
            &json!({ "seed_memory_id": seed_id, "depth": depth_v.clone(), "kinds": kinds_v.clone(), "activeOnly": true }),
            db,
        )?;
        let nodes = g.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let edges = g.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        (nodes, edges, "concentric")
    } else {
        let nodes = crate::db_json::query_rows(db, "SELECT * FROM memory.memories WHERE superseded_by IS NULL ORDER BY id", &[])?
            .as_array()
            .cloned()
            .unwrap_or_default();
        let ids: HashSet<i64> = nodes.iter().filter_map(|n| n.get("id").and_then(|v| v.as_i64())).collect();
        let all_links = crate::db_json::query_rows(db, "SELECT * FROM memory.memory_links", &[])?
            .as_array()
            .cloned()
            .unwrap_or_default();
        let links: Vec<Value> = if truthy(Some(&kinds_v)) {
            let allow: Vec<String> = kinds_v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
            all_links.into_iter().filter(|l| allow.iter().any(|a| a == l.get("kind").and_then(|v| v.as_str()).unwrap_or(""))).collect()
        } else {
            all_links
        };
        let edges: Vec<Value> = links
            .into_iter()
            .filter(|l| {
                let from = l.get("from_memory_id").and_then(|v| v.as_i64());
                let to = l.get("to_memory_id").and_then(|v| v.as_i64());
                from.map_or(false, |f| ids.contains(&f)) && to.map_or(false, |t| ids.contains(&t))
            })
            .collect();
        (nodes, edges, "grid")
    };

    if nodes.is_empty() {
        return Ok(json!(format!(
            "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>Memory graph</title><style>{}</style></head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"./index.html\" style=\"color:#58a6ff;text-decoration:none;\">\u{2190} antiloki docs</a></nav><h1>Memory graph</h1><div class=\"empty\">No memories to render.</div></body></html>",
            CSS
        )));
    }

    let positions = if layout_mode == "concentric" {
        layout_concentric(&nodes, &edges, seed_id_opt.unwrap())
    } else {
        layout_grid(&nodes)
    };

    let title_suffix = match seed_id_opt {
        Some(id) => format!("\u{b7} seed #{} \u{b7} depth {}", id, tmpl(Some(&depth_v))),
        None => format!("\u{b7} all ({} nodes, {} edges)", nodes.len(), edges.len()),
    };

    let body = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Memory graph {ts}</title>\n<style>{css}</style>\n</head>\n<body><nav style=\"padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,<body>quot;Segoe UI<body>quot;,sans-serif;font-size:0.85em;\"><a href=\"./index.html\" style=\"color:#58a6ff;text-decoration:none;\">\u{2190} antiloki docs</a></nav>\n<h1>Memory graph <span style=\"color:#88ddff\">{ts}</span></h1>\n<p class=\"meta\">layout: {lm} \u{b7} hover any node or edge for details</p>\n{legend}\n<div class=\"svg-wrap\">{svg}</div>\n{detail}\n</body>\n</html>",
        ts = title_suffix,
        css = CSS,
        lm = layout_mode,
        legend = LEGEND_HTML,
        svg = render_svg(&positions, &nodes, &edges),
        detail = render_detail_list(&nodes)
    );
    Ok(json!(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT, situation TEXT, guidance TEXT, validated INTEGER, superseded_by INTEGER);
             CREATE TABLE memory.memory_links (id INTEGER PRIMARY KEY, from_memory_id INTEGER NOT NULL, to_memory_id INTEGER NOT NULL, kind TEXT NOT NULL, rationale TEXT, weight REAL NOT NULL DEFAULT 1.0);",
        )
        .unwrap();
        c
    }

    // grid mode is trig-free (sqrt/ceil are IEEE-754-exact) — safe to assert byte-for-byte.
    #[test]
    fn empty_graph_renders_the_empty_page_verbatim() {
        let c = db();
        let out = memoryGraphHtml(&json!({}), &c).unwrap();
        let s = out.as_str().unwrap();
        assert!(s.starts_with("<!DOCTYPE html>\n<html><head>"));
        assert!(s.contains("<div class=\"empty\">No memories to render.</div></body></html>"));
        assert!(!s.contains("<h1>Memory graph <span"));
    }

    #[test]
    fn grid_mode_two_nodes_exact() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,type,title,situation,guidance,validated) VALUES (1,'lesson','A','s1','g1',1),(2,'correction','B','s2','g2',0)", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind,rationale,weight) VALUES (1,1,2,'contradicts',NULL,1)", []).unwrap();
        let out = memoryGraphHtml(&json!({}), &c).unwrap();
        let s = out.as_str().unwrap();
        assert!(s.contains("<title>Memory graph \u{b7} all (2 nodes, 1 edges)</title>"));
        assert!(s.contains("<p class=\"meta\">layout: grid \u{b7} hover any node or edge for details</p>"));
        assert!(s.contains("<line x1=\"-75\" y1=\"0\" x2=\"75\" y2=\"0\" stroke=\"#ff6b6b\" stroke-width=\"2.5\" opacity=\"0.7\" stroke-dasharray=\"4 3\">\n  <title>contradicts </title>\n</line>"));
        assert!(s.contains("<g transform=\"translate(-75,0)\">"));
        assert!(s.contains("<text class=\"node-id\" text-anchor=\"middle\" y=\"-4\">#1 \u{2713}</text>"));
        assert!(s.contains("<text class=\"node-id\" text-anchor=\"middle\" y=\"-4\">#2 </text>"));
    }

    #[test]
    fn grid_mode_drops_edge_to_superseded_node_entirely() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,type,title,situation,guidance,validated,superseded_by) VALUES (1,'lesson','A','s1','g1',1,NULL),(2,'decision','B','s2','g2',1,99)", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind,rationale,weight) VALUES (1,1,2,'related',NULL,1)", []).unwrap();
        let out = memoryGraphHtml(&json!({}), &c).unwrap();
        let s = out.as_str().unwrap();
        assert!(s.contains("(1 nodes, 0 edges)"));
        assert!(!s.contains("<line"));
    }

    #[test]
    fn grid_mode_kinds_filter_and_unknown_fallback_colors_and_truncate_and_escaping() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,type,title,situation,guidance,validated) VALUES (1,'lesson','A very long title that goes past the twenty eight char limit for sure','s1 <x>','g1 & g2',0),(2,'weird_type','B','s2','g2',1)", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind,rationale,weight) VALUES (1,1,2,'weird_kind',NULL,1)", []).unwrap();
        let out = memoryGraphHtml(&json!({}), &c).unwrap();
        let s = out.as_str().unwrap();
        // unknown type/kind both fall back to '#888' (not '#888888', which is the known 'related'
        // color — always present in the static legend, so we assert presence of the fallback
        // rather than absence of '#888888').
        assert!(s.contains("fill=\"#888\""));
        assert!(s.contains("stroke=\"#888\""));
        // truncate(28) cuts BEFORE escaping (raw char count), then the cut label is escaped
        assert!(s.contains("<text class=\"node-label\" text-anchor=\"middle\" y=\"10\">A very long title that goes\u{2026}</text>"));
        assert!(s.contains("when:</span> s1 &lt;x&gt;"));
        assert!(s.contains("do:</span> g1 &amp; g2"));

        let filtered = memoryGraphHtml(&json!({ "kinds": ["extends"] }), &c).unwrap();
        assert!(filtered.as_str().unwrap().contains("(2 nodes, 0 edges)"));
    }

    // concentric depth=0 short-circuits before any cos/sin call — safe to assert byte-for-byte.
    #[test]
    fn concentric_depth_zero_is_trig_free_and_exact() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,type,title,situation,guidance,validated) VALUES (1,'lesson','Seed','sit','guide',1)", []).unwrap();
        let out = memoryGraphHtml(&json!({ "seed_memory_id": 1, "depth": 0 }), &c).unwrap();
        let s = out.as_str().unwrap();
        assert!(s.contains("<title>Memory graph \u{b7} seed #1 \u{b7} depth 0</title>"));
        assert!(s.contains("<p class=\"meta\">layout: concentric \u{b7} hover any node or edge for details</p>"));
        assert!(s.contains("<svg viewBox=\"-150 -150 300 300\" width=\"100%\" height=\"300\" xmlns=\"http://www.w3.org/2000/svg\">\n\n<g transform=\"translate(0,0)\">"));
    }

    // depth>=1 exercises the real ring math (cos/sin) — assert STRUCTURE only, not the exact
    // decimal digits of a trig result (see module doc: platform libm can differ from V8 fdlibm).
    #[test]
    fn concentric_depth_one_places_a_neighbor_and_keeps_a_dangling_edge_for_excluded_targets() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,type,title,situation,guidance,validated,superseded_by) VALUES (1,'lesson','A','s1','g1',1,NULL),(2,'decision','B','s2','g2',1,99)", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind,rationale,weight) VALUES (1,1,2,'related',NULL,1)", []).unwrap();
        let out = memoryGraphHtml(&json!({ "seed_memory_id": 1, "depth": 1 }), &c).unwrap();
        let s = out.as_str().unwrap();
        // node 2 is superseded -> excluded from `nodes` (only 1 <g> block) but the edge to it is
        // still drawn (memoryGraph keeps the edge as an audit trail even when its target is filtered).
        assert_eq!(s.matches("<g transform").count(), 1);
        assert!(s.contains("<line x1=\"0\" y1=\"0\""));
        assert!(s.contains("Nodes (1)"));
    }

    #[test]
    fn kinds_empty_array_is_truthy_and_blocks_every_edge_in_concentric_mode() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,type,title,situation,guidance,validated) VALUES (1,'lesson','A','s1','g1',1),(2,'decision','B','s2','g2',0)", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind,rationale,weight) VALUES (1,1,2,'related',NULL,1)", []).unwrap();
        let out = memoryGraphHtml(&json!({ "seed_memory_id": 1, "kinds": [] }), &c).unwrap();
        let s = out.as_str().unwrap();
        assert!(s.contains("<title>Memory graph \u{b7} seed #1 \u{b7} depth 3</title>")); // depth defaults to 3
        assert_eq!(s.matches("<g transform").count(), 1); // only the seed — the neighbor never reached
        assert!(!s.contains("<line"));
    }
}
