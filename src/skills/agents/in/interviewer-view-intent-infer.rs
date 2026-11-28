//! interviewer-view-intent-infer.rs — twin of skills/agents/in/interviewer-view-intent-infer.mjs
//! (standard lane, ignores db). NAMES ARE CONTRACT: `interviewerViewIntentInfer` (default).
//! Deterministic fallback for view_intent/view_data/presentation_md when the AI presenter is empty.
// QUALITY: 80/100 — board/card_detail/table/scalar renderers off the first ok dispatch result
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS String(v): scalars verbatim, null → "null", arrays comma-join, plain object → "[object Object]".
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
// JS `${v}` interpolation: absent → "undefined", else String(v).
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(x) => js_string(x) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }
fn stringify(v: &Value) -> String { serde_json::to_string(v).unwrap_or_else(|_| "null".into()) } // JSON.stringify
// JS property access `obj[k]` / `arr[k]` (string key parsed to index for arrays).
fn get_prop<'a>(v: &'a Value, k: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(k),
        Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get(i)),
        _ => None,
    }
}
// JS Object.keys(v): object → insertion-order keys; array → index strings; else [].
fn object_keys(v: &Value) -> Vec<String> {
    match v {
        Value::Object(m) => m.keys().cloned().collect(),
        Value::Array(a) => (0..a.len()).map(|i| i.to_string()).collect(),
        _ => vec![],
    }
}

// `${c.title ?? '(no title)'}`
fn title_or(c: &Value) -> String {
    match nz(c.get("title")) { Some(t) => tmpl(Some(t)), None => "(no title)".into() }
}
fn status_is(c: &Value, want: &str) -> bool { c.get("status").and_then(|v| v.as_str()) == Some(want) }

fn infer_from_card_list(result: Option<&Value>) -> Option<Value> {
    // cards = Array.isArray(result) ? result : (Array.isArray(result?.cards) ? result.cards : null)
    let cards: &Vec<Value> = match result {
        Some(Value::Array(a)) => a,
        Some(v) => match v.get("cards") { Some(Value::Array(a)) => a, _ => return None },
        None => return None,
    };
    let (mut backlog, mut doing_n, mut done, mut archived) = (0i64, 0i64, 0i64, 0i64);
    for c in cards {
        match c.get("status").and_then(|v| v.as_str()) {
            Some("backlog") => backlog += 1,
            Some("doing") => doing_n += 1,
            Some("done") => done += 1,
            Some("archived") => archived += 1,
            _ => {} // 'other'/unknown counted in JS but never rendered
        }
    }
    let n = cards.len();
    let mut lines: Vec<String> = vec![
        format!("**Board** \u{2014} {} card{}", n, if n == 1 { "" } else { "s" }),
        format!("\u{25D0} doing: {} \u{00B7} \u{25CB} backlog: {} \u{00B7} \u{25CF} done: {} \u{00B7} \u{2715} archived: {}", doing_n, backlog, done, archived),
    ];
    let doing: Vec<&Value> = cards.iter().filter(|c| status_is(c, "doing")).take(5).collect();
    if !doing.is_empty() {
        lines.push(String::new());
        lines.push("_in flight:_".into());
        for c in &doing { lines.push(format!("- `{}` \u{2014} {}", tmpl(c.get("slug")), title_or(c))); }
    } else {
        let next: Vec<&Value> = cards.iter().filter(|c| status_is(c, "backlog")).take(3).collect();
        if !next.is_empty() {
            lines.push(String::new());
            lines.push("_next up:_".into());
            for c in &next { lines.push(format!("- `{}` \u{2014} {}", tmpl(c.get("slug")), title_or(c))); }
        }
    }
    let mut out = Map::new();
    out.insert("view_intent".into(), json!("board"));
    out.insert("view_data".into(), json!({ "cards": cards }));
    out.insert("presentation_md".into(), json!(lines.join("\n")));
    Some(Value::Object(out))
}

fn infer_from_card_get(result: Option<&Value>) -> Option<Value> {
    // if (!result || typeof result !== 'object') return null;
    let result = result?;
    if !truthy(Some(result)) { return None; }
    if !(result.is_object() || result.is_array()) { return None; }
    // const card = result.card ?? result;
    let card: &Value = nz(result.get("card")).unwrap_or(result);
    // if (!card?.slug) return null;
    if !truthy(card.get("slug")) { return None; }
    let slug = card.get("slug").unwrap();
    let title_part = if truthy(card.get("title")) { format!("\u{2014} {}", tmpl(card.get("title"))) } else { String::new() };
    let line1 = format!("**`{}`** {}", tmpl(Some(slug)), title_part);
    let status_disp = match nz(card.get("status")) { Some(s) => tmpl(Some(s)), None => "(unknown)".into() };
    let line2 = format!("status: `{}`", status_disp);
    let mut vd = Map::new();
    vd.insert("slug".into(), slug.clone());
    if let Some(t) = card.get("title") { vd.insert("title".into(), t.clone()); } // undefined → omitted
    if let Some(s) = card.get("status") { vd.insert("status".into(), s.clone()); }
    let mut out = Map::new();
    out.insert("view_intent".into(), json!("card_detail"));
    out.insert("view_data".into(), Value::Object(vd));
    out.insert("presentation_md".into(), json!(format!("{}\n{}", line1, line2)));
    Some(Value::Object(out))
}

fn infer_from_generic_array(result: Option<&Value>, action: &Value) -> Option<Value> {
    let arr = match result { Some(Value::Array(a)) if !a.is_empty() => a, _ => return None };
    let sample = &arr[0];
    if !(sample.is_object() || sample.is_array()) { return None; } // typeof === 'object' && != null
    let columns: Vec<String> = object_keys(sample).into_iter().take(5).collect();
    let rows: Vec<Value> = arr.iter().take(20).map(|r| {
        let cells: Vec<Value> = columns.iter().map(|k| {
            let v = if r.is_null() { None } else { get_prop(r, k) };
            let cell = match v {
                None | Some(Value::Null) => String::new(),
                Some(x) if x.is_object() || x.is_array() => take_chars(&stringify(x), 40),
                Some(x) => take_chars(&js_string(x), 60),
            };
            json!(cell)
        }).collect();
        json!(cells)
    }).collect();
    let n = arr.len();
    let mut out = Map::new();
    out.insert("view_intent".into(), json!("table"));
    out.insert("view_data".into(), json!({ "columns": columns, "rows": rows }));
    out.insert("presentation_md".into(), json!(format!("**{}** \u{2014} {} row{}", tmpl(Some(action)), n, if n == 1 { "" } else { "s" })));
    Some(Value::Object(out))
}

fn infer_from_scalar(result: Option<&Value>, action: &Value) -> Value {
    // if (result == null) → ok
    if matches!(result, None | Some(Value::Null)) {
        return json!({ "view_intent": null, "view_data": null, "presentation_md": format!("\u{2713} `{}` ok", tmpl(Some(action))) });
    }
    let result = result.unwrap();
    if result.is_object() || result.is_array() {
        let keys = object_keys(result);
        if keys.len() <= 6 {
            let mut lines: Vec<String> = vec![format!("**`{}`** ok", tmpl(Some(action)))];
            for k in keys.iter().take(6) {
                let rendered = match get_prop(result, k) {
                    // typeof v === 'object' (incl null) → JSON.stringify; else String(v)
                    Some(v) if v.is_object() || v.is_array() || v.is_null() => take_chars(&stringify(v), 80),
                    Some(v) => take_chars(&js_string(v), 80),
                    None => String::new(), // unreachable: k comes from object_keys(result)
                };
                lines.push(format!("- {}: `{}`", k, rendered));
            }
            return json!({ "view_intent": null, "view_data": null, "presentation_md": lines.join("\n") });
        }
    }
    json!({ "view_intent": "code", "view_data": null, "presentation_md": format!("**`{}`** returned data \u{2014} see below", tmpl(Some(action))) })
}

pub fn interviewerViewIntentInfer(input: &Value, _db: &Connection) -> Result<Value, String> {
    let dispatch = match input.get("dispatch_results") { Some(Value::Array(a)) => a, _ => {
        return Ok(json!({ "view_intent": null, "view_data": null, "presentation_md": null }));
    }};
    if dispatch.is_empty() {
        return Ok(json!({ "view_intent": null, "view_data": null, "presentation_md": null }));
    }
    let first = match dispatch.iter().find(|d| truthy(d.get("ok"))) {
        Some(f) => f,
        None => {
            let ff = &dispatch[0];
            let err = match nz(ff.get("error")) { Some(e) => tmpl(Some(e)), None => "unknown error".into() };
            return Ok(json!({ "view_intent": null, "view_data": null, "presentation_md": format!("\u{274C} `{}` failed: {}", tmpl(ff.get("action")), err) }));
        }
    };
    let action = nz(first.get("action")).cloned().unwrap_or(json!("")); // first.action ?? ''
    let result = first.get("result"); // Option = undefined-aware
    let action_str = action.as_str();

    if action_str == Some("board-keeper.card.list") || action_str == Some("system.card.list") {
        if let Some(v) = infer_from_card_list(result) { return Ok(v); }
    }
    if action_str == Some("board-keeper.card.get") || action_str == Some("system.card.get")
        || action_str == Some("board-keeper.card.read") || action_str == Some("system.card.read") {
        if let Some(v) = infer_from_card_get(result) { return Ok(v); }
    }
    if let Some(v) = infer_from_generic_array(result, &action) { return Ok(v); }
    Ok(infer_from_scalar(result, &action))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { interviewerViewIntentInfer(&i, &Connection::open_in_memory().unwrap()).unwrap() }

    #[test]
    fn renders() {
        // empty dispatch → all null
        assert_eq!(call(json!({ "dispatch_results": [] })), json!({ "view_intent": null, "view_data": null, "presentation_md": null }));
        assert_eq!(call(json!({})), json!({ "view_intent": null, "view_data": null, "presentation_md": null }));

        // board
        let cards = json!([
            { "slug": "A-1", "title": "first", "status": "doing" },
            { "slug": "A-2", "title": "second", "status": "backlog" },
            { "slug": "A-3", "title": "third", "status": "done" },
        ]);
        let b = call(json!({ "dispatch_results": [{ "ok": true, "action": "board-keeper.card.list", "result": cards }] }));
        assert_eq!(b["view_intent"], json!("board"));
        assert_eq!(b["view_data"]["cards"].as_array().unwrap().len(), 3);
        assert!(b["presentation_md"].as_str().unwrap().contains("**Board** \u{2014} 3 cards"));
        assert!(b["presentation_md"].as_str().unwrap().contains("doing: 1"));
        assert!(b["presentation_md"].as_str().unwrap().contains("_in flight:_"));
        assert!(b["presentation_md"].as_str().unwrap().contains("- `A-1` \u{2014} first"));

        // wrapper {cards:[...]}, backlog-only → next up
        let b2 = call(json!({ "dispatch_results": [{ "ok": true, "action": "system.card.list", "result": { "cards": [{ "slug": "X", "status": "backlog" }] } }] }));
        assert_eq!(b2["view_intent"], json!("board"));
        assert!(b2["presentation_md"].as_str().unwrap().contains("_next up:_"));
        assert!(b2["presentation_md"].as_str().unwrap().contains("- `X` \u{2014} (no title)"));

        // card_detail (no title → trailing space, view_data omits nothing-present)
        let d = call(json!({ "dispatch_results": [{ "ok": true, "action": "board-keeper.card.get", "result": { "slug": "Y-1", "status": "backlog" } }] }));
        assert_eq!(d["view_intent"], json!("card_detail"));
        assert_eq!(d["view_data"], json!({ "slug": "Y-1", "status": "backlog" })); // title absent → omitted
        assert!(d["presentation_md"].as_str().unwrap().contains("**`Y-1`** \nstatus: `backlog`"));

        // table
        let t = call(json!({ "dispatch_results": [{ "ok": true, "action": "system.memory.search", "result": [{ "id": 1, "title": "a", "score": 0.9 }, { "id": 2, "title": "b" }] }] }));
        assert_eq!(t["view_intent"], json!("table"));
        assert_eq!(t["view_data"]["columns"], json!(["id", "title", "score"]));
        assert_eq!(t["view_data"]["rows"], json!([["1", "a", "0.9"], ["2", "b", ""]]));
        assert_eq!(t["presentation_md"], json!("**system.memory.search** \u{2014} 2 rows"));

        // scalar object (<=6 keys)
        let s = call(json!({ "dispatch_results": [{ "ok": true, "action": "system.memory.add", "result": { "memory_id": 42, "type": "decision" } }] }));
        assert_eq!(s["view_intent"], Value::Null);
        assert!(s["presentation_md"].as_str().unwrap().contains("- memory_id: `42`"));

        // scalar object (>6 keys) → code
        let big = call(json!({ "dispatch_results": [{ "ok": true, "action": "x.y", "result": { "a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7 } }] }));
        assert_eq!(big["view_intent"], json!("code"));
        assert!(big["presentation_md"].as_str().unwrap().contains("returned data \u{2014} see below"));

        // null result → ok
        let ok = call(json!({ "dispatch_results": [{ "ok": true, "action": "z", "result": null }] }));
        assert_eq!(ok["presentation_md"], json!("\u{2713} `z` ok"));

        // all failed
        let f = call(json!({ "dispatch_results": [{ "ok": false, "action": "system.broken", "error": "boom" }] }));
        assert_eq!(f["view_intent"], Value::Null);
        assert_eq!(f["presentation_md"], json!("\u{274C} `system.broken` failed: boom"));
        // failed with no error → 'unknown error'
        let f2 = call(json!({ "dispatch_results": [{ "ok": false, "action": "a.b" }] }));
        assert_eq!(f2["presentation_md"], json!("\u{274C} `a.b` failed: unknown error"));
    }
}
