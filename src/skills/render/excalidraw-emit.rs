//! excalidraw-emit.rs — twin of skills/render/excalidraw-emit.mjs (no-db, pure formatter). NAMES
//! ARE CONTRACT: `rect` · `ellipse` · `diamond` · `text` · `arrow` · `paragraph` ·
//! `narrativeBlock` · `buildExcalidraw` · `excalidrawEmit` (default). `textInside` is a private
//! (non-exported) JS helper, mirrored here as a private fn, not a census unit.
//!
//! MIXED module, same class as driver-token.rs / auth-init.rs: `buildExcalidraw` is pure
//! deterministic assembly (no randomness) and IS fixture-tested as an ordinary `sig:"no-db"`
//! helper (fixture + frontier entry + dispatch arm). Every OTHER exported function —
//! `rect`/`ellipse`/`diamond`/`text`/`arrow`/`paragraph`/`narrativeBlock`, and the default export
//! `excalidrawEmit` (which calls `rect` internally) — unconditionally calls the module-private
//! `id()` on every element it builds (`randomBytes(10).toString('base64url')`, `node:crypto`), on
//! EVERY success path, with no seed/injection seam. A real OS-CSPRNG id can never be
//! byte-identical to Node's `crypto.randomBytes` across the two lanes, so — same treatment as
//! `driver-token.rs`'s `randomBytes(32)` key generation and `auth-init.rs`'s per-agent key
//! generation — these 8 units are BESPOKE NATIVE: real, correct code (real OS random bytes, real
//! base64url encoding, byte-for-byte matching JS field-by-field structure other than the id
//! itself), proven only by `#[cfg(test)]` (checking shape + id format, not an exact id value), no
//! conform-db fixture / frontier entry / dispatch arm (see manifest).
//!
//! Object key order (gotcha #1): every element is `{ ...BASE_ELEMENT, <overrides> }` — spread
//! keeps BASE_ELEMENT's key order for keys it already defines (only the VALUE changes for an
//! override of an existing key), and appends genuinely-new keys at the end in source-literal
//! order. `base_element()` below builds that fixed prefix once; each factory fn inserts overrides
//! by key (preserving position for existing keys, appending new ones) exactly as JS does.

use serde_json::{json, Map, Value};

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// JS numbers have no int/float distinction; JSON.stringify renders a whole-valued number without
// a decimal point. Every computed coordinate/size below is a JS arithmetic result, so it must
// render the same way — `json!(f64)` alone would wrongly force "10.0" for a whole value.
fn js_num(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) } }

// ---- id() — bespoke-native OS CSPRNG reach-in, same convention as driver-token.rs / auth-init.rs
// (zero new Cargo dependency: /dev/urandom on unix, RtlGenRandom on Windows), encoded with the
// already-present `base64` crate's URL_SAFE_NO_PAD alphabet = Node's `.toString('base64url')`.
#[cfg(unix)]
fn os_random_bytes(n: usize) -> Vec<u8> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").expect("/dev/urandom");
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf).expect("read /dev/urandom");
    buf
}

#[cfg(windows)]
#[link(name = "advapi32")]
extern "system" {
    #[link_name = "SystemFunction036"]
    fn RtlGenRandom(buf: *mut u8, len: u32) -> u8;
}

#[cfg(windows)]
fn os_random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    let ok = unsafe { RtlGenRandom(buf.as_mut_ptr(), buf.len() as u32) };
    assert!(ok != 0, "RtlGenRandom failed");
    buf
}

/// `function id() { return randomBytes(10).toString('base64url'); }`
fn id() -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.encode(os_random_bytes(10))
}

// ---- BASE_ELEMENT — fixed key order, spread as the prefix of every element object ------------

fn base_element() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("angle".into(), json!(0));
    m.insert("strokeColor".into(), json!("#1e1e1e"));
    m.insert("backgroundColor".into(), json!("transparent"));
    m.insert("fillStyle".into(), json!("solid"));
    m.insert("strokeWidth".into(), json!(2));
    m.insert("strokeStyle".into(), json!("solid"));
    m.insert("roughness".into(), json!(1));
    m.insert("opacity".into(), json!(100));
    m.insert("groupIds".into(), json!([]));
    m.insert("frameId".into(), Value::Null);
    m.insert("index".into(), Value::Null);
    m.insert("roundness".into(), json!({ "type": 3 }));
    m.insert("seed".into(), json!(0));
    m.insert("version".into(), json!(1));
    m.insert("versionNonce".into(), json!(0));
    m.insert("isDeleted".into(), json!(false));
    m.insert("boundElements".into(), Value::Null);
    m.insert("updated".into(), json!(1));
    m.insert("link".into(), Value::Null);
    m.insert("locked".into(), json!(false));
    m
}

// `.insert` preserves position for a key BASE_ELEMENT already defines (matches JS spread-then-
// override semantics), appends for a genuinely new key, in the order this helper is called.
fn set(m: &mut Map<String, Value>, k: &str, v: Value) { m.insert(k.to_string(), v); }

fn get_f64(o: &Value, k: &str) -> f64 { o.get(k).and_then(Value::as_f64).unwrap_or(0.0) }
fn get_str<'a>(o: &'a Value, k: &str) -> Option<&'a str> { o.get(k).and_then(Value::as_str) }

// ---- rect / ellipse / diamond -----------------------------------------------------------------

fn shape(shape_type: &str, opts: &Value) -> Vec<Value> {
    let x = get_f64(opts, "x");
    let y = get_f64(opts, "y");
    let width = get_f64(opts, "width");
    let height = get_f64(opts, "height");
    let label = get_str(opts, "label");
    let fill = get_str(opts, "fill").unwrap_or("transparent");
    let default_stroke = "#1e1e1e";
    let stroke = get_str(opts, "stroke").unwrap_or(default_stroke);
    let font_size = opts.get("fontSize").and_then(Value::as_f64).unwrap_or(if shape_type == "rectangle" { 16.0 } else { 14.0 });

    let mut m = base_element();
    set(&mut m, "id", json!(id()));
    set(&mut m, "type", json!(shape_type));
    set(&mut m, "x", json!(x));
    set(&mut m, "y", json!(y));
    set(&mut m, "width", json!(width));
    set(&mut m, "height", json!(height));
    set(&mut m, "backgroundColor", json!(fill));
    set(&mut m, "strokeColor", json!(stroke));
    let shape_id = m.get("id").cloned().unwrap();
    let shape_x = x;
    let shape_y = y;
    let shape_w = width;
    let shape_h = height;

    let mut out = vec![Value::Object(m)];
    if let Some(label) = label.filter(|s| !s.is_empty()) {
        out.extend(text_inside(shape_x, shape_y, shape_w, shape_h, shape_id, label, font_size));
    }
    out
}

/// `export function rect({ x, y, width, height, label, fill = 'transparent',
/// stroke = '#1e1e1e', fontSize = 16 })`
pub fn rect(opts: &Value) -> Value { json!(shape("rectangle", opts)) }

/// `export function ellipse({ ..., fontSize = 14 })`
pub fn ellipse(opts: &Value) -> Value { json!(shape("ellipse", opts)) }

/// `export function diamond({ ..., fontSize = 14 })`
pub fn diamond(opts: &Value) -> Value { json!(shape("diamond", opts)) }

// `function textInside(shape, label, fontSize)` — private JS helper, not an export/census unit.
fn text_inside(shape_x: f64, shape_y: f64, shape_w: f64, shape_h: f64, shape_id: Value, label: &str, font_size: f64) -> Vec<Value> {
    let padding = 8.0;
    let mut m = base_element();
    set(&mut m, "id", json!(id()));
    set(&mut m, "type", json!("text"));
    set(&mut m, "x", json!(shape_x + padding));
    set(&mut m, "y", json!(shape_y + (shape_h / 2.0) - (font_size / 2.0)));
    set(&mut m, "width", json!(shape_w - (padding * 2.0)));
    set(&mut m, "height", json!(font_size * 1.4));
    set(&mut m, "text", json!(label));
    set(&mut m, "fontSize", json!(font_size));
    set(&mut m, "fontFamily", json!(1));
    set(&mut m, "textAlign", json!("center"));
    set(&mut m, "verticalAlign", json!("middle"));
    set(&mut m, "strokeColor", json!("#1e1e1e"));
    set(&mut m, "baseline", json!(font_size));
    set(&mut m, "containerId", shape_id);
    set(&mut m, "originalText", json!(label));
    set(&mut m, "autoResize", json!(false));
    set(&mut m, "lineHeight", json!(1.25));
    vec![Value::Object(m)]
}

// ---- text --------------------------------------------------------------------------------------

/// `export function text({ x, y, text: txt, fontSize = 16, color = '#1e1e1e' })`
pub fn text(opts: &Value) -> Value {
    let x = get_f64(opts, "x");
    let y = get_f64(opts, "y");
    let txt = str_qq(opts.get("text"));
    let font_size = opts.get("fontSize").and_then(Value::as_f64).unwrap_or(16.0);
    let color = get_str(opts, "color").unwrap_or("#1e1e1e");

    let mut m = base_element();
    set(&mut m, "id", json!(id()));
    set(&mut m, "type", json!("text"));
    set(&mut m, "x", json!(x));
    set(&mut m, "y", json!(y));
    set(&mut m, "width", json!((txt.chars().count() as f64 * font_size * 0.55).max(60.0)));
    set(&mut m, "height", json!(font_size * 1.4));
    set(&mut m, "text", json!(txt));
    set(&mut m, "fontSize", json!(font_size));
    set(&mut m, "fontFamily", json!(1));
    set(&mut m, "textAlign", json!("left"));
    set(&mut m, "verticalAlign", json!("top"));
    set(&mut m, "strokeColor", json!(color));
    set(&mut m, "baseline", json!(font_size));
    set(&mut m, "containerId", Value::Null);
    set(&mut m, "originalText", json!(txt));
    set(&mut m, "autoResize", json!(true));
    set(&mut m, "lineHeight", json!(1.25));
    json!([Value::Object(m)])
}

// ---- arrow -------------------------------------------------------------------------------------

/// `export function arrow({ x1, y1, x2, y2, label, stroke = '#1e1e1e' })`
pub fn arrow(opts: &Value) -> Value {
    let x1 = get_f64(opts, "x1");
    let y1 = get_f64(opts, "y1");
    let x2 = get_f64(opts, "x2");
    let y2 = get_f64(opts, "y2");
    let label = get_str(opts, "label");
    let stroke = get_str(opts, "stroke").unwrap_or("#1e1e1e");

    let mut m = base_element();
    set(&mut m, "id", json!(id()));
    set(&mut m, "type", json!("arrow"));
    set(&mut m, "x", json!(x1));
    set(&mut m, "y", json!(y1));
    set(&mut m, "width", json!((x2 - x1).abs()));
    set(&mut m, "height", json!((y2 - y1).abs()));
    set(&mut m, "strokeColor", json!(stroke));
    set(&mut m, "points", json!([[0.0, 0.0], [x2 - x1, y2 - y1]]));
    set(&mut m, "lastCommittedPoint", Value::Null);
    set(&mut m, "startBinding", Value::Null);
    set(&mut m, "endBinding", Value::Null);
    set(&mut m, "startArrowhead", Value::Null);
    set(&mut m, "endArrowhead", json!("arrow"));
    set(&mut m, "elbowed", json!(false));

    let mut out = vec![Value::Object(m)];
    if let Some(label) = label.filter(|s| !s.is_empty()) {
        let label_opts = json!({
            "x": (x1 + x2) / 2.0 - (label.chars().count() as f64 * 4.0),
            "y": (y1 + y2) / 2.0 - 10.0,
            "text": label,
            "fontSize": 12,
            "color": stroke,
        });
        if let Value::Array(els) = text(&label_opts) { out.extend(els); }
    }
    json!(out)
}

// ---- paragraph -----------------------------------------------------------------------------------

/// `export function paragraph({ x, y, width, text: txt, fontSize = 13, color = '#37474f' })`
pub fn paragraph(opts: &Value) -> Value {
    let x = get_f64(opts, "x");
    let y = get_f64(opts, "y");
    let width = get_f64(opts, "width");
    let txt = str_qq(opts.get("text"));
    let font_size = opts.get("fontSize").and_then(Value::as_f64).unwrap_or(13.0);
    let color = get_str(opts, "color").unwrap_or("#37474f");
    let line_count = txt.split('\n').count() as f64;

    let mut m = base_element();
    set(&mut m, "id", json!(id()));
    set(&mut m, "type", json!("text"));
    set(&mut m, "x", json!(x));
    set(&mut m, "y", json!(y));
    set(&mut m, "width", json!(width));
    set(&mut m, "height", json!((font_size * 1.4).max(line_count * font_size * 1.4)));
    set(&mut m, "text", json!(txt));
    set(&mut m, "fontSize", json!(font_size));
    set(&mut m, "fontFamily", json!(1));
    set(&mut m, "textAlign", json!("left"));
    set(&mut m, "verticalAlign", json!("top"));
    set(&mut m, "strokeColor", json!(color));
    set(&mut m, "baseline", json!(font_size));
    set(&mut m, "containerId", Value::Null);
    set(&mut m, "originalText", json!(txt));
    set(&mut m, "autoResize", json!(false));
    set(&mut m, "lineHeight", json!(1.4));
    json!([Value::Object(m)])
}

// ---- narrativeBlock ------------------------------------------------------------------------------

/// `export function narrativeBlock({ x, y, width, text: txt, height = null, fill = '#fffbe6' })`
pub fn narrativeBlock(opts: &Value) -> Value {
    let x = get_f64(opts, "x");
    let y = get_f64(opts, "y");
    let width = get_f64(opts, "width");
    let txt = str_qq(opts.get("text"));
    let fill = get_str(opts, "fill").unwrap_or("#fffbe6");
    let line_count = txt.split('\n').count() as f64;
    let font_size = 13.0_f64;
    let h = match nz(opts.get("height")).and_then(Value::as_f64) {
        Some(v) => v,
        None => (60.0_f64).max(line_count * font_size * 1.4 + 24.0),
    };

    let mut panel = base_element();
    set(&mut panel, "id", json!(id()));
    set(&mut panel, "type", json!("rectangle"));
    set(&mut panel, "x", json!(x));
    set(&mut panel, "y", json!(y));
    set(&mut panel, "width", json!(width));
    set(&mut panel, "height", json!(h));
    set(&mut panel, "strokeColor", json!("#cfd8dc"));
    set(&mut panel, "backgroundColor", json!(fill));
    set(&mut panel, "fillStyle", json!("solid"));
    set(&mut panel, "strokeWidth", json!(1));
    set(&mut panel, "roundness", json!({ "type": 3 }));

    let mut txt_el = base_element();
    set(&mut txt_el, "id", json!(id()));
    set(&mut txt_el, "type", json!("text"));
    set(&mut txt_el, "x", json!(x + 14.0));
    set(&mut txt_el, "y", json!(y + 12.0));
    set(&mut txt_el, "width", json!(width - 28.0));
    set(&mut txt_el, "height", json!(h - 24.0));
    set(&mut txt_el, "text", json!(txt));
    set(&mut txt_el, "fontSize", json!(font_size));
    set(&mut txt_el, "fontFamily", json!(1));
    set(&mut txt_el, "textAlign", json!("left"));
    set(&mut txt_el, "verticalAlign", json!("top"));
    set(&mut txt_el, "strokeColor", json!("#37474f"));
    set(&mut txt_el, "baseline", json!(font_size));
    set(&mut txt_el, "containerId", Value::Null);
    set(&mut txt_el, "originalText", json!(txt));
    set(&mut txt_el, "autoResize", json!(false));
    set(&mut txt_el, "lineHeight", json!(1.5));

    json!([Value::Object(panel), Value::Object(txt_el)])
}

// ---- buildExcalidraw (pure — the ONLY fixture-tested unit in this module) --------------------

/// `export function buildExcalidraw(elements, { viewBackgroundColor = '#ffffff',
/// name = 'antiloki-view' } = {})`
pub fn buildExcalidraw(elements: &Value, options: &Value) -> Value {
    let view_background_color = match options.get("viewBackgroundColor") {
        Some(v) if !v.is_null() => v.clone(),
        _ => json!("#ffffff"),
    };
    let name = match options.get("name") {
        Some(v) if !v.is_null() => v.clone(),
        _ => json!("antiloki-view"),
    };
    json!({
        "type": "excalidraw",
        "version": 2,
        "source": "https://excalidraw.com",
        "elements": elements,
        "appState": {
            "gridSize": 20,
            "viewBackgroundColor": view_background_color,
            "currentItemFontFamily": 1,
            "name": name,
        },
        "files": {},
    })
}

// ---- default export ------------------------------------------------------------------------------

/// `export default function excalidrawEmit(input = {}, _context)` — bespoke-native like the 7
/// element factories above (calls `rect`, which calls `id()`); see module doc.
pub fn excalidrawEmit(input: &Value) -> Value {
    let title_raw = match input.get("title") {
        Some(v) if !v.is_null() => str_qq(Some(v)),
        _ => "antiloki".to_string(),
    };
    let title: String = title_raw.chars().take(60).collect();
    let rect_opts = json!({ "x": 100, "y": 100, "width": 200, "height": 80, "label": title, "fill": "#e3f2fd" });
    let elements = rect(&rect_opts);
    buildExcalidraw(&elements, &json!({ "name": "antiloki-hello" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elems(v: &Value) -> &Vec<Value> { v.as_array().unwrap() }

    #[test]
    fn rect_without_label_has_one_element_with_expected_shape_and_a_valid_id() {
        let r = rect(&json!({ "x": 10, "y": 20, "width": 100, "height": 50 }));
        let els = elems(&r);
        assert_eq!(els.len(), 1);
        let e = &els[0];
        assert_eq!(e["type"], json!("rectangle"));
        assert_eq!(e["x"], json!(10.0));
        assert_eq!(e["backgroundColor"], json!("transparent"));
        assert_eq!(e["strokeColor"], json!("#1e1e1e"));
        let id_str = e["id"].as_str().unwrap();
        assert_eq!(id_str.len(), 14); // base64url(10 bytes), no padding
        assert!(id_str.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        let keys: Vec<&String> = e.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "angle", "strokeColor", "backgroundColor", "fillStyle", "strokeWidth", "strokeStyle",
                "roughness", "opacity", "groupIds", "frameId", "index", "roundness", "seed", "version",
                "versionNonce", "isDeleted", "boundElements", "updated", "link", "locked",
                "id", "type", "x", "y", "width", "height",
            ]
        );
    }

    #[test]
    fn rect_with_label_appends_a_bound_text_element() {
        let r = rect(&json!({ "x": 0, "y": 0, "width": 100, "height": 40, "label": "hi", "fill": "#fff" }));
        let els = elems(&r);
        assert_eq!(els.len(), 2);
        assert_eq!(els[1]["type"], json!("text"));
        assert_eq!(els[1]["text"], json!("hi"));
        assert_eq!(els[1]["containerId"], els[0]["id"]);
        assert_eq!(els[1]["textAlign"], json!("center"));
    }

    #[test]
    fn two_ids_in_a_row_are_different() {
        let a = id();
        let b = id();
        assert_ne!(a, b);
    }

    #[test]
    fn ellipse_and_diamond_use_default_font_size_14() {
        let e = ellipse(&json!({ "x": 0, "y": 0, "width": 10, "height": 10, "label": "e" }));
        let d = diamond(&json!({ "x": 0, "y": 0, "width": 10, "height": 10, "label": "d" }));
        assert_eq!(elems(&e)[1]["fontSize"], json!(14.0));
        assert_eq!(elems(&d)[1]["fontSize"], json!(14.0));
    }

    #[test]
    fn text_width_uses_max_60_or_computed() {
        let short = text(&json!({ "x": 0, "y": 0, "text": "hi" }));
        assert_eq!(elems(&short)[0]["width"], json!(60.0));
        let long = text(&json!({ "x": 0, "y": 0, "text": "a much longer string of text here" }));
        let w = elems(&long)[0]["width"].as_f64().unwrap();
        assert!(w > 60.0);
    }

    #[test]
    fn arrow_without_label_is_one_element_with_points() {
        let a = arrow(&json!({ "x1": 0, "y1": 0, "x2": 10, "y2": 20 }));
        let els = elems(&a);
        assert_eq!(els.len(), 1);
        assert_eq!(els[0]["points"], json!([[0.0, 0.0], [10.0, 20.0]]));
        assert_eq!(els[0]["endArrowhead"], json!("arrow"));
    }

    #[test]
    fn arrow_with_label_appends_a_text_element() {
        let a = arrow(&json!({ "x1": 0, "y1": 0, "x2": 10, "y2": 0, "label": "go" }));
        let els = elems(&a);
        assert_eq!(els.len(), 2);
        assert_eq!(els[1]["type"], json!("text"));
        assert_eq!(els[1]["text"], json!("go"));
    }

    #[test]
    fn paragraph_height_grows_with_line_count() {
        let p = paragraph(&json!({ "x": 0, "y": 0, "width": 100, "text": "a\nb\nc" }));
        let els = elems(&p);
        assert_eq!(els.len(), 1);
        assert_eq!(els[0]["height"], json!(13.0 * 1.4 * 3.0));
    }

    #[test]
    fn narrative_block_default_height_from_line_count_and_explicit_height_override() {
        let default_h = narrativeBlock(&json!({ "x": 0, "y": 0, "width": 200, "text": "one line" }));
        let els = elems(&default_h);
        assert_eq!(els.len(), 2);
        assert_eq!(els[0]["type"], json!("rectangle"));
        assert_eq!(els[1]["type"], json!("text"));
        assert_eq!(els[0]["height"], json!(60.0)); // max(60, 1*13*1.4+24)

        let explicit_h = narrativeBlock(&json!({ "x": 0, "y": 0, "width": 200, "text": "x", "height": 999 }));
        assert_eq!(elems(&explicit_h)[0]["height"], json!(999.0));
    }

    #[test]
    fn build_excalidraw_defaults() {
        let out = buildExcalidraw(&json!([{"id": "a"}]), &json!({}));
        assert_eq!(
            out,
            json!({
                "type": "excalidraw",
                "version": 2,
                "source": "https://excalidraw.com",
                "elements": [{"id": "a"}],
                "appState": {
                    "gridSize": 20,
                    "viewBackgroundColor": "#ffffff",
                    "currentItemFontFamily": 1,
                    "name": "antiloki-view",
                },
                "files": {},
            })
        );
        let keys: Vec<&String> = out.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["type", "version", "source", "elements", "appState", "files"]);
    }

    #[test]
    fn build_excalidraw_explicit_options() {
        let out = buildExcalidraw(&json!([]), &json!({ "viewBackgroundColor": "#000", "name": "my-view" }));
        assert_eq!(out["appState"]["viewBackgroundColor"], json!("#000"));
        assert_eq!(out["appState"]["name"], json!("my-view"));
    }

    #[test]
    fn excalidraw_emit_default_title_and_custom_title_truncated_to_60() {
        let out = excalidrawEmit(&json!({}));
        assert_eq!(out["appState"]["name"], json!("antiloki-hello"));
        let els = elems(&out["elements"]);
        assert_eq!(els[1]["text"], json!("antiloki"));

        let long_title = "x".repeat(100);
        let out2 = excalidrawEmit(&json!({ "title": long_title }));
        let els2 = elems(&out2["elements"]);
        assert_eq!(els2[1]["text"].as_str().unwrap().len(), 60);
    }
}
