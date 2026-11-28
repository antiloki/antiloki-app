//! excalidraw-embed.rs — twin of skills/docs/excalidraw-embed.mjs (standard lane + 2 no-db
//! helpers). NAMES ARE CONTRACT: `excalidrawEmbed` (default) · `layoutNodes` · `escapeXml`.
//!
//! Hand-drawn-style SVG renderer, no deps beyond `err.type` (VALIDATION_TYPE) from
//! skills/engine/error.mjs, reproduced inline as an exact-message throw since the AntilokiError
//! class itself is deferred to the dispatch-layer crossing (see skills/engine/error.rs header).
//!
//! rng fidelity: `mulberry32` is a 32-bit-int PRNG (uses `|0`, `>>>`, `Math.imul` in the JS) —
//! ported here with `i32`/`u32` wrapping arithmetic so the byte sequence of jitter/path offsets
//! is bit-identical to V8 for the same seed.
// QUALITY: 75/100 — deterministic PRNG + trig layout ported by hand; f64 trig (cos/sin) may
// diverge from V8 in the last ULP on some platforms, which toFixed(1)-style rounding usually
// absorbs but is not a hard guarantee.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

const DEFAULT_W: i64 = 800;
const DEFAULT_H: i64 = 500;
const NODE_R: f64 = 30.0;

const DEFS_STYLE: &str = "<defs><style>\n    .node-bg { fill:#1f2530; stroke:#58a6ff; stroke-width:1.5; }\n    .node-label { fill:#e6edf3; font-family:\"Comic Sans MS\",\"Marker Felt\",\"Bradley Hand\",cursive; font-size:13px; text-anchor:middle; dominant-baseline:middle; }\n    .edge { stroke:#8b949e; stroke-width:1.5; fill:none; stroke-linecap:round; }\n    .edge-label { fill:#8b949e; font-family:\"Comic Sans MS\",cursive; font-size:10px; text-anchor:middle; }\n    .kind-label { fill:#8b949e; font-family:cursive; font-size:11px; }\n  </style></defs>";

// ── standard helpers (playbook, copied verbatim) ─────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}

// = JS `typeof v` — used to reproduce err.type's exact `(got ${typeof got})` message fragment.
fn js_typeof(v: &Value) -> &'static str {
    match v {
        Value::Null => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "object",
        Value::Object(_) => "object",
    }
}

// = JS `seed >>> 0` (ToUint32): truncate toward zero, wrap mod 2^32, non-negative.
fn js_to_uint32(n: f64) -> u32 {
    if !n.is_finite() { return 0; }
    n.trunc().rem_euclid(4294967296.0) as u32
}

// JSON.stringify(NaN|Infinity) === null — mirror that when embedding a computed f64 as a Value.
fn json_number(f: f64) -> Value {
    serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
}

// Map/edge-endpoint key for JS `new Map(...).get(x)` (SameValueZero) over JSON-primitive ids.
// Objects/arrays as ids use reference equality in JS (never matches a freshly-built node) — None.
fn map_key(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(format!("s:{s}")),
        Value::Number(n) => Some(format!("n:{n}")),
        Value::Bool(b) => Some(format!("b:{b}")),
        Value::Null => Some("null".to_string()),
        _ => None,
    }
}
// JS: a node with no `id` property (or an edge with no `from`/`to`) reads as `undefined`,
// which is itself a valid, distinct Map key (SameValueZero) — not "absent from the map".
// A missing property (None here) must still yield an insert/lookup key so such nodes/edges
// can match each other, same as they would via a real JS `Map` keyed by `n.id`. Only
// object/array ids (reference-equality in JS, map_key -> None) remain genuinely unmatchable.
fn map_key_opt(v: Option<&Value>) -> Option<String> {
    match v {
        None => Some("u:undefined".to_string()),
        Some(x) => map_key(x),
    }
}

// ── mulberry32: 32-bit-int PRNG, ported with wrapping i32/u32 arithmetic to match V8 exactly ──
fn imul(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b)
}

fn mulberry32(seed: u32) -> impl FnMut() -> f64 {
    let mut t: i32 = seed as i32;
    move || {
        t = t.wrapping_add(0x6D2B79F5u32 as i32);
        let a = t ^ (((t as u32) >> 15) as i32);
        let b = 1 | t;
        let mut r = imul(a, b);
        let a2 = r ^ (((r as u32) >> 7) as i32);
        let b2 = 61 | r;
        let add = imul(a2, b2);
        r ^= r.wrapping_add(add);
        let shifted = ((r as u32) >> 14) as i32;
        let x = (r ^ shifted) as u32;
        (x as f64) / 4294967296.0
    }
}

fn jitter(rng: &mut impl FnMut() -> f64, base: f64, amount: f64) -> f64 {
    base + (rng() - 0.5) * 2.0 * amount
}

// String(s ?? '').replace(/[&<>"']/g, ...) — single pass.
fn escape_xml_str(s: Option<&Value>) -> String {
    let src = str_qq(s);
    let mut out = String::with_capacity(src.len());
    for c in src.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

// `{ ...node }` — non-object spread (number/bool/null/array) yields no own enumerable string
// keys relevant here, so it degenerates to an empty object (string-spread's index props are not
// a realistic node shape and are intentionally not modeled).
fn spread_node(node: &Value) -> serde_json::Map<String, Value> {
    match node {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    }
}

fn layout_nodes_internal(nodes: &[Value], w: &Value, h: &Value) -> Vec<Value> {
    let n = nodes.len();
    if n == 0 {
        return vec![];
    }
    let wf = js_number(Some(w));
    let hf = js_number(Some(h));
    let cx = wf / 2.0;
    let cy = hf / 2.0;
    if n == 1 {
        let mut m = spread_node(&nodes[0]);
        m.insert("x".to_string(), json_number(cx));
        m.insert("y".to_string(), json_number(cy));
        return vec![Value::Object(m)];
    }
    let ring_size: usize = 8;
    // `Math.min(w, h)` propagates NaN in JS; `f64::min` does not (IEEE minNum semantics return
    // the non-NaN operand when exactly one side is NaN) — force NaN-propagation to match.
    let radius = (if wf.is_nan() || hf.is_nan() { f64::NAN } else { wf.min(hf) }) * 0.32;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let ring = i / ring_size;
        let pos = i % ring_size;
        let ring_count = ring_size.min(n - ring * ring_size);
        let angle = (pos as f64 / ring_count as f64) * std::f64::consts::PI * 2.0 + (ring as f64) * 0.3;
        let r = radius * (1.0 + (ring as f64) * 0.55);
        let mut m = spread_node(&nodes[i]);
        m.insert("x".to_string(), json_number(cx + angle.cos() * r));
        m.insert("y".to_string(), json_number(cy + angle.sin() * r));
        out.push(Value::Object(m));
    }
    out
}

fn path_between(x1: f64, y1: f64, x2: f64, y2: f64, rng: &mut impl FnMut() -> f64) -> String {
    let mx = (x1 + x2) / 2.0 + (rng() - 0.5) * 12.0;
    let my = (y1 + y2) / 2.0 + (rng() - 0.5) * 12.0;
    format!("M {x1:.1} {y1:.1} Q {mx:.1} {my:.1} {x2:.1} {y2:.1}")
}

fn arrow_head(x: f64, y: f64, dx: f64, dy: f64, color: &str) -> String {
    let len_raw = (dx * dx + dy * dy).sqrt();
    // JS `Math.sqrt(...) || 1`: falls back to 1 for 0 (falsy) or NaN (falsy).
    let len = if len_raw == 0.0 || len_raw.is_nan() { 1.0 } else { len_raw };
    let ux = dx / len;
    let uy = dy / len;
    let angle: f64 = 0.45;
    let ax = x - 10.0 * (ux * angle.cos() - uy * angle.sin());
    let ay = y - 10.0 * (ux * angle.sin() + uy * angle.cos());
    let bx = x - 10.0 * (ux * (-angle).cos() - uy * (-angle).sin());
    let by = y - 10.0 * (ux * (-angle).sin() + uy * (-angle).cos());
    format!(
        r#"<polyline points="{ax:.1},{ay:.1} {x:.1},{y:.1} {bx:.1},{by:.1}" fill="none" stroke="{color}" stroke-width="1.5" stroke-linecap="round" />"#
    )
}

/// no-db helper: `layoutNodes(nodes, w, h)`.
pub fn layoutNodes(nodes: &Value, w: &Value, h: &Value) -> Value {
    let arr: Vec<Value> = nodes.as_array().cloned().unwrap_or_default();
    json!(layout_nodes_internal(&arr, w, h))
}

/// no-db helper: `escapeXml(s)`.
pub fn escapeXml(s: &Value) -> Value {
    json!(escape_xml_str(Some(s)))
}

/// standard lane (db unused): `excalidrawEmbed(input, context)`.
pub fn excalidrawEmbed(input: &Value, _db: &Connection) -> Result<Value, String> {
    let base = nz(Some(input)).cloned().unwrap_or_else(|| json!({}));
    let obj = base.as_object();
    let field = |k: &str| obj.and_then(|o| o.get(k)).cloned();

    let kind = field("kind").unwrap_or_else(|| json!("generic"));
    let nodes_v = field("nodes").unwrap_or_else(|| json!([]));
    let edges_v = field("edges").unwrap_or_else(|| json!([]));
    let width = field("width").unwrap_or_else(|| json!(DEFAULT_W));
    let height = field("height").unwrap_or_else(|| json!(DEFAULT_H));
    let seed = field("seed").unwrap_or_else(|| json!(42));

    let nodes: Vec<Value> = match &nodes_v {
        Value::Array(a) => a.clone(),
        other => return Err(format!("excalidraw-embed: nodes must be array (got {})", js_typeof(other))),
    };
    let edges: Vec<Value> = match &edges_v {
        Value::Array(a) => a.clone(),
        other => return Err(format!("excalidraw-embed: edges must be array (got {})", js_typeof(other))),
    };

    let mut rng = mulberry32(js_to_uint32(js_number(Some(&seed))));
    let positioned = layout_nodes_internal(&nodes, &width, &height);

    let mut by_id: HashMap<String, (f64, f64)> = HashMap::new();
    for n in &positioned {
        if let Value::Object(m) = n {
            if let Some(key) = map_key_opt(m.get("id")) {
                let x = m.get("x").and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
                let y = m.get("y").and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
                by_id.insert(key, (x, y));
            }
        }
    }

    let mut parts: Vec<String> = Vec::new();
    parts.push(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}" style="max-width:100%; height:auto; background:#161b22; border:1px solid #30363d; border-radius:6px;">"#,
        tmpl(Some(&width)),
        tmpl(Some(&height))
    ));
    parts.push(DEFS_STYLE.to_string());
    parts.push(format!(
        r#"<text x="14" y="22" class="kind-label">[{}]</text>"#,
        escape_xml_str(Some(&kind))
    ));

    for e in &edges {
        let eo = e.as_object();
        let from = eo.and_then(|o| o.get("from"));
        let to = eo.and_then(|o| o.get("to"));
        let a = map_key_opt(from).and_then(|k| by_id.get(&k)).copied();
        let b = map_key_opt(to).and_then(|k| by_id.get(&k)).copied();
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        let color_v = eo.and_then(|o| o.get("color")).cloned();
        let color = nz(color_v.as_ref()).cloned().unwrap_or_else(|| json!("#8b949e"));
        let color_s = tmpl(Some(&color));
        parts.push(format!(
            r#"<path d="{}" class="edge" stroke="{}" />"#,
            path_between(a.0, a.1, b.0, b.1, &mut rng),
            color_s
        ));
        parts.push(arrow_head(b.0, b.1, b.0 - a.0, b.1 - a.1, &color_s));
        let label_v = eo.and_then(|o| o.get("label")).cloned();
        if truthy(label_v.as_ref()) {
            let mx = (a.0 + b.0) / 2.0;
            let my = (a.1 + b.1) / 2.0 - 6.0;
            parts.push(format!(
                r#"<text x="{mx:.1}" y="{my:.1}" class="edge-label">{}</text>"#,
                escape_xml_str(label_v.as_ref())
            ));
        }
    }

    for n in &positioned {
        let no = n.as_object();
        let color_v = no.and_then(|o| o.get("color")).cloned();
        let stroke = nz(color_v.as_ref()).cloned().unwrap_or_else(|| json!("#58a6ff"));
        let stroke_s = tmpl(Some(&stroke));
        let rx = jitter(&mut rng, NODE_R, 1.5);
        let ry = jitter(&mut rng, NODE_R, 1.5);
        let nx = no.and_then(|o| o.get("x")).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
        let ny = no.and_then(|o| o.get("y")).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
        parts.push(format!(
            r#"<ellipse cx="{nx:.1}" cy="{ny:.1}" rx="{rx:.1}" ry="{ry:.1}" class="node-bg" stroke="{stroke_s}" />"#
        ));
        let label_field = no.and_then(|o| o.get("label"));
        let id_field = no.and_then(|o| o.get("id"));
        // `n.label ?? n.id`: `??` only inspects whether the LEFT operand is null/undefined;
        // when it is, it evaluates to the RIGHT operand exactly AS-IS — including when that
        // right operand is itself explicitly null (`String(null) === "null"`) or itself
        // absent (`String(undefined) === "undefined"`). It never normalizes null and
        // undefined into each other, so "label absent, id null" ("null") and "label null,
        // id absent" ("undefined") are genuinely different outcomes that must stay
        // distinguished here rather than collapsed via a null-stripping helper like `nz`.
        let label_text: String = match label_field {
            Some(v) if !v.is_null() => js_string(v),
            _ => match id_field {
                Some(Value::Null) => "null".to_string(),
                Some(v) => js_string(v),
                None => "undefined".to_string(),
            },
        }
        .chars()
        .take(14)
        .collect();
        parts.push(format!(
            r#"<text x="{nx:.1}" y="{ny:.1}" class="node-label">{}</text>"#,
            escape_xml_str(Some(&json!(label_text)))
        ));
    }

    parts.push("</svg>".to_string());
    Ok(json!(parts.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn layout_nodes_empty_single_two() {
        assert_eq!(layoutNodes(&json!([]), &json!(800), &json!(500)), json!([]));

        let single = layoutNodes(&json!([{"id":"a","label":"A"}]), &json!(800), &json!(500));
        let arr = single.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], json!("a"));
        assert!((arr[0]["x"].as_f64().unwrap() - 400.0).abs() < 1e-9);
        assert!((arr[0]["y"].as_f64().unwrap() - 250.0).abs() < 1e-9);

        let two = layoutNodes(&json!([{"id":"a"},{"id":"b"}]), &json!(800), &json!(500));
        let arr2 = two.as_array().unwrap();
        assert!((arr2[0]["x"].as_f64().unwrap() - 560.0).abs() < 1e-6);
        assert!((arr2[0]["y"].as_f64().unwrap() - 250.0).abs() < 1e-6);
        assert!((arr2[1]["x"].as_f64().unwrap() - 240.0).abs() < 1e-6);
        assert!((arr2[1]["y"].as_f64().unwrap() - 250.0).abs() < 1e-6);
    }

    #[test]
    fn escape_xml_reserved_and_defaults() {
        assert_eq!(escapeXml(&json!("&<>\"'")), json!("&amp;&lt;&gt;&quot;&apos;"));
        assert_eq!(escapeXml(&json!("plain")), json!("plain"));
        assert_eq!(escapeXml(&Value::Null), json!(""));
        assert_eq!(escapeXml(&json!(5)), json!("5"));
    }

    #[test]
    fn minimal_svg_shell_and_kind_escaping() {
        let svg = excalidrawEmbed(&json!({"nodes": [], "edges": []}), &db()).unwrap();
        let s = svg.as_str().unwrap();
        assert!(s.starts_with(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 500""#));
        assert!(s.contains(r#"<text x="14" y="22" class="kind-label">[generic]</text>"#));
        assert!(s.trim_end().ends_with("</svg>"));

        let escaped = excalidrawEmbed(&json!({"kind": "<a>", "nodes": [], "edges": []}), &db()).unwrap();
        assert!(escaped.as_str().unwrap().contains("[&lt;a&gt;]"));
    }

    #[test]
    fn edge_missing_endpoint_is_skipped() {
        let svg = excalidrawEmbed(
            &json!({"nodes": [{"id":"a","label":"A"}], "edges": [{"from":"a","to":"missing","label":"rel"}]}),
            &db(),
        )
        .unwrap();
        let s = svg.as_str().unwrap();
        assert!(!s.contains(r#"class="edge""#));
        assert!(!s.contains(r#"class="edge-label">rel<"#));
    }

    #[test]
    fn throws_match_v8_message_on_non_array_nodes_edges() {
        let err1 = excalidrawEmbed(&json!({"nodes": "x", "edges": []}), &db()).unwrap_err();
        assert_eq!(err1, "excalidraw-embed: nodes must be array (got string)");
        let err2 = excalidrawEmbed(&json!({"nodes": [], "edges": null}), &db()).unwrap_err();
        assert_eq!(err2, "excalidraw-embed: edges must be array (got object)");
    }

    // Golden output captured from the real JS (node --input-type=module) for a fixed seed —
    // pins mulberry32 + jitter + pathBetween + arrowHead byte-for-byte against V8.
    #[test]
    fn deterministic_golden_output_matches_js() {
        let svg = excalidrawEmbed(
            &json!({
                "kind": "agent-grants",
                "nodes": [{"id":"a","label":"A"},{"id":"b","label":"B"}],
                "edges": [{"from":"a","to":"b","label":"rel"}],
                "seed": 7
            }),
            &db(),
        )
        .unwrap();
        let expected = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 800 500\" style=\"max-width:100%; height:auto; background:#161b22; border:1px solid #30363d; border-radius:6px;\">\n<defs><style>\n    .node-bg { fill:#1f2530; stroke:#58a6ff; stroke-width:1.5; }\n    .node-label { fill:#e6edf3; font-family:\"Comic Sans MS\",\"Marker Felt\",\"Bradley Hand\",cursive; font-size:13px; text-anchor:middle; dominant-baseline:middle; }\n    .edge { stroke:#8b949e; stroke-width:1.5; fill:none; stroke-linecap:round; }\n    .edge-label { fill:#8b949e; font-family:\"Comic Sans MS\",cursive; font-size:10px; text-anchor:middle; }\n    .kind-label { fill:#8b949e; font-family:cursive; font-size:11px; }\n  </style></defs>\n<text x=\"14\" y=\"22\" class=\"kind-label\">[agent-grants]</text>\n<path d=\"M 560.0 250.0 Q 394.1 244.7 240.0 250.0\" class=\"edge\" stroke=\"#8b949e\" />\n<polyline points=\"249.0,254.3 240.0,250.0 249.0,245.7\" fill=\"none\" stroke=\"#8b949e\" stroke-width=\"1.5\" stroke-linecap=\"round\" />\n<text x=\"400.0\" y=\"244.0\" class=\"edge-label\">rel</text>\n<ellipse cx=\"560.0\" cy=\"250.0\" rx=\"31.4\" ry=\"30.6\" class=\"node-bg\" stroke=\"#58a6ff\" />\n<text x=\"560.0\" y=\"250.0\" class=\"node-label\">A</text>\n<ellipse cx=\"240.0\" cy=\"250.0\" rx=\"30.1\" ry=\"29.7\" class=\"node-bg\" stroke=\"#58a6ff\" />\n<text x=\"240.0\" y=\"250.0\" class=\"node-label\">B</text>\n</svg>";
        assert_eq!(svg.as_str().unwrap(), expected);
    }

    // `n.label ?? n.id` truth table (verified live in node): `??` falls through to the right
    // operand AS-IS on a null/undefined left operand — it does not normalize null<->undefined.
    #[test]
    fn label_id_fallback_truth_table() {
        let cases: &[(Value, &str)] = &[
            (json!({}), "undefined"), // label absent, id absent
            (json!({"label": null, "id": null}), "null"), // label null, id null
            (json!({"id": null}), "null"), // label absent, id null
            (json!({"label": null}), "undefined"), // label null, id absent
            (json!({"label": null, "id": "real-id"}), "real-id"), // label null, id real
            (json!({"id": "real-id2"}), "real-id2"), // label absent, id real
            (json!({"label": "real-label", "id": "ignored"}), "real-label"), // label real
        ];
        for (node, expected) in cases {
            let svg = excalidrawEmbed(&json!({"nodes": [node.clone()], "edges": []}), &db()).unwrap();
            let s = svg.as_str().unwrap();
            let needle = format!(r#"class="node-label">{expected}</text>"#);
            assert!(s.contains(&needle), "expected {needle:?} in svg for node {node:?}, got: {s}");
        }
    }
}
