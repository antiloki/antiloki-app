//! THE TRANSFORM NODE (RFC 0011 §12): one node, modes — deterministic JSON/text transforms, no model.
use crate::team_check;
use serde_json::{json, Map, Value};

pub const MODES: [&str; 11] = ["edit_fields", "filter", "sort", "aggregate", "split", "limit", "dedupe", "template", "format_json", "sanitize", "rules"];

pub fn apply(mode: &str, input: Value, config: &Value) -> Result<Value, String> {
    if !MODES.contains(&mode) { return Err(format!("`{mode}` is not a transform mode; they are {}", MODES.join(", "))); }
    match mode {
        "edit_fields" => edit_fields(input, config),
        "filter" => filter(input, config),
        "sort" => sort(input, config),
        "aggregate" => aggregate(input, config),
        "split" => split(input, config),
        "limit" => limit(input, config),
        "dedupe" => dedupe(input, config),
        "template" => template(input, config),
        "format_json" => format_json(input, config),
        "sanitize" => sanitize(input, config),
        "rules" => rules(input, config),
        _ => unreachable!(),
    }
}

fn obj(v: Value) -> Result<Map<String, Value>, String> {
    v.as_object().cloned().ok_or_else(|| "expected a JSON object".into())
}

fn edit_fields(input: Value, config: &Value) -> Result<Value, String> {
    let mut m = obj(input)?;
    if let Some(pick) = config.get("pick").and_then(Value::as_array) {
        let keys: Vec<&str> = pick.iter().filter_map(|k| k.as_str()).collect();
        m = m.into_iter().filter(|(k, _)| keys.iter().any(|p| p == k)).collect();
    }
    if let Some(rename) = config.get("rename").and_then(Value::as_object) {
        let mut out = Map::new();
        for (k, v) in m {
            let nk = rename.get(&k).and_then(Value::as_str).unwrap_or(&k).to_string();
            out.insert(nk, v);
        }
        m = out;
    }
    if let Some(set) = config.get("set").and_then(Value::as_object) {
        for (k, v) in set { m.insert(k.clone(), v.clone()); }
    }
    Ok(Value::Object(m))
}

fn filter(input: Value, config: &Value) -> Result<Value, String> {
    let arr = input.as_array().ok_or("filter expects a list")?;
    let field = config["field"].as_str().ok_or("say `field`")?;
    let eq = config.get("eq").cloned().unwrap_or(Value::Null);
    Ok(json!(arr.iter().filter(|x| x.get(field) == Some(&eq)).cloned().collect::<Vec<_>>()))
}

fn sort(input: Value, config: &Value) -> Result<Value, String> {
    let mut arr = input.as_array().cloned().ok_or("sort expects a list")?;
    let by = config["by"].as_str().unwrap_or("id");
    let desc = config["desc"].as_bool().unwrap_or(false);
    arr.sort_by(|a, b| {
        let ka = a.get(by);
        let kb = b.get(by);
        let ord = match (ka, kb) {
            (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64().partial_cmp(&y.as_f64()).unwrap_or(std::cmp::Ordering::Equal),
            (Some(Value::String(x)), Some(Value::String(y))) => x.cmp(y),
            (Some(x), Some(y)) => x.to_string().cmp(&y.to_string()),
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        };
        if desc { ord.reverse() } else { ord }
    });
    Ok(json!(arr))
}

fn aggregate(input: Value, config: &Value) -> Result<Value, String> {
    let arr = input.as_array().ok_or("aggregate expects a list")?;
    let group = config["group_by"].as_str().ok_or("say `group_by`")?;
    let op = config["op"].as_str().unwrap_or("count");
    let mut out = Map::new();
    for item in arr {
        let key = match item.get(group) { Some(Value::String(t)) => t.clone(), Some(v) => v.to_string(), None => "null".into() };
        let e = out.entry(key).or_insert(json!({ "count": 0, "sum": 0.0 }));
        e["count"] = json!(e["count"].as_u64().unwrap_or(0) + 1);
        if op == "sum" {
            let v = item.get(config["field"].as_str().unwrap_or("value")).and_then(Value::as_f64).unwrap_or(0.0);
            e["sum"] = json!(e["sum"].as_f64().unwrap_or(0.0) + v);
        }
    }
    Ok(Value::Object(out))
}

fn split(input: Value, config: &Value) -> Result<Value, String> {
    match config["by"].as_str().unwrap_or("items") {
        "heading" => {
            let text = input.as_str().ok_or("split by heading expects text")?;
            let mut sections = vec![];
            let mut title = String::new();
            let mut body = vec![];
            for line in text.lines() {
                if line.starts_with("# ") {
                    if !title.is_empty() || !body.is_empty() {
                        sections.push(json!({ "title": title, "body": body.join("\n") }));
                    }
                    title = line[2..].trim().to_string();
                    body.clear();
                } else {
                    body.push(line);
                }
            }
            if !title.is_empty() || !body.is_empty() {
                sections.push(json!({ "title": title, "body": body.join("\n") }));
            }
            Ok(json!(sections))
        }
        "items" => {
            let n = config["n"].as_u64().ok_or("split by items needs `n`")? as usize;
            let arr = input.as_array().ok_or("split by items expects a list")?;
            Ok(json!(arr.chunks(n).map(|c| json!(c)).collect::<Vec<_>>()))
        }
        "file" => {
            let text = input.as_str().ok_or("split by file expects text")?;
            Ok(json!(text.split("\n---\n").map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>()))
        }
        other => Err(format!("split by `{other}` is not built")),
    }
}

fn limit(input: Value, config: &Value) -> Result<Value, String> {
    let n = config["n"].as_u64().ok_or("say `n`")? as usize;
    match input {
        Value::Array(a) => Ok(json!(a.into_iter().take(n).collect::<Vec<_>>())),
        Value::String(s) => Ok(json!(s.chars().take(n).collect::<String>())),
        _ => Err("limit expects a list or text".into()),
    }
}

fn dedupe(input: Value, config: &Value) -> Result<Value, String> {
    let arr = input.as_array().ok_or("dedupe expects a list")?;
    let by = config.get("by").and_then(Value::as_str);
    let mut seen = std::collections::BTreeSet::new();
    let mut out = vec![];
    for item in arr {
        let key = if let Some(f) = by { item.get(f).map(|v| v.to_string()).unwrap_or_else(|| item.to_string()) } else { item.to_string() };
        if seen.insert(key) { out.push(item.clone()); }
    }
    Ok(json!(out))
}

fn template(input: Value, config: &Value) -> Result<Value, String> {
    let tpl = config["template"].as_str().ok_or("say `template`")?;
    let m = obj(input)?;
    let mut out = tpl.to_string();
    for (k, v) in &m {
        let slot = format!("{{{k}}}");
        let rep = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out = out.replace(&slot, &rep);
    }
    Ok(json!(out))
}

fn format_json(input: Value, _config: &Value) -> Result<Value, String> {
    serde_json::to_string_pretty(&input).map(|s| json!(s)).map_err(|e| e.to_string())
}

fn sanitize(input: Value, _config: &Value) -> Result<Value, String> {
    Ok(sanitize_value(input))
}

fn sanitize_value(v: Value) -> Value {
    match v {
        Value::String(s) => Value::String(crate::ledger::sanitize(&s)),
        Value::Array(a) => Value::Array(a.into_iter().map(sanitize_value).collect()),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, x)| (k, sanitize_value(x))).collect()),
        other => other,
    }
}

// ── RULES (VID-18, operator 2026-11-17): declarative rules for any output, from any model or none ─────────────────────────────────
// `fields`: {name: {trim (default true), map: {"near miss": "wanted"}, enum: [..], nearest: bool, default: "x", case: upper|lower|title, max: N}}.
// Order per field: trim, map, enum (exact, then the nearest when `nearest`, else the default, else an error naming the allowed values), case, max length (cut at a word).
// The answer is the fixed fields plus `fixes`: what the rules changed, so a run shows what a model got wrong. `keep_others: true` carries the other fields through.
fn norm_key(s: &str) -> String { s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect() }
fn levenshtein(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect()); let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() { let mut cur = vec![i]; for j in 1..=b.len() { cur.push((prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]))); } prev = cur; }
    prev[b.len()]
}
/// The allowed value a raw one means: the exact one (ignoring case, spaces, punctuation), else, when `nearest`, one that contains or is contained (3+ letters), else the closest by edit distance within a third of its length. Pure.
pub fn resolve_enum(v: &str, list: &[String], nearest: bool) -> Option<(String, &'static str)> {
    let k = norm_key(v); if k.is_empty() { return None; }
    if let Some(e) = list.iter().find(|e| norm_key(e) == k) { return Some((e.clone(), "exact")); }
    if !nearest { return None; }
    let part: Vec<&String> = list.iter().filter(|e| { let ek = norm_key(e); ek.len() >= 3 && k.len() >= 3 && (k.contains(&ek) || ek.contains(&k)) }).collect();
    if part.len() == 1 { return Some((part[0].clone(), "contains")); }
    let best = list.iter().map(|e| (levenshtein(&k, &norm_key(e)), e)).min_by_key(|(d, _)| *d)?;
    if best.0 as f64 <= (k.len().max(norm_key(best.1).len()) as f64 / 3.0).floor().max(1.0) { Some((best.1.clone(), "near")) } else { None }
}
fn cut_at_word(v: &str, max: usize) -> String {
    if v.chars().count() <= max { return v.to_string(); }
    let head: String = v.chars().take(max).collect();
    let cut = match head.rfind(' ') { Some(i) if i >= max / 2 => head[..i].to_string(), _ => head };
    cut.trim_end_matches(|c: char| " ,;:-–.".contains(c)).to_string()
}
fn rules(input: Value, config: &Value) -> Result<Value, String> {
    let m = obj(input)?; let fields = config["fields"].as_object().ok_or("rules: say `fields`")?;
    let (mut out, mut fixes): (Map<String, Value>, Vec<String>) = (Map::new(), vec![]);
    for (name, rule) in fields {
        let raw = m.get(name).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default(); let mut v = raw.clone();
        if rule["trim"].as_bool().unwrap_or(true) { v = v.trim().trim_matches(|c: char| c == '"' || c == '`').trim().to_string(); if v != raw { fixes.push(format!("{name}: trimmed")); } }
        if let Some(map) = rule["map"].as_object() { let k = norm_key(&v); if let Some(to) = map.iter().find(|(a, _)| norm_key(a) == k).and_then(|(_, b)| b.as_str()) { fixes.push(format!("{name}: \"{v}\" -> \"{to}\" (map)")); v = to.to_string(); } }
        if let Some(en) = rule["enum"].as_array() {
            let list: Vec<String> = en.iter().filter_map(Value::as_str).map(String::from).collect();
            match resolve_enum(&v, &list, rule["nearest"].as_bool().unwrap_or(false)) {
                Some((c, how)) => { if c != v { fixes.push(format!("{name}: \"{v}\" -> \"{c}\" ({how})")); } v = c; }
                None => match rule["default"].as_str() { Some(d) => { fixes.push(format!("{name}: \"{v}\" is not allowed, used the default \"{d}\"")); v = d.to_string(); } None => return Err(format!("{name}: \"{v}\" is not one of {}", list.join(", "))) },
            }
        } else if v.is_empty() { if let Some(d) = rule["default"].as_str() { fixes.push(format!("{name}: empty, used the default \"{d}\"")); v = d.to_string(); } }
        let before = v.clone();
        v = match rule["case"].as_str() { Some("upper") => v.to_uppercase(), Some("lower") => v.to_lowercase(), Some("title") => v.split(' ').map(|w| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str().to_lowercase().as_str()).unwrap_or_default() }).collect::<Vec<_>>().join(" "), _ => v };
        if v != before { fixes.push(format!("{name}: case")); }
        if let Some(max) = rule["max"].as_u64() { let cut = cut_at_word(&v, max as usize); if cut != v { fixes.push(format!("{name}: cut to {max} characters")); v = cut; } }
        out.insert(name.clone(), json!(v));
    }
    if config["keep_others"].as_bool().unwrap_or(false) { for (k, v) in &m { if !out.contains_key(k) { out.insert(k.clone(), v.clone()); } } }
    out.insert("fixes".into(), json!(fixes)); Ok(Value::Object(out))
}

pub fn check(mode: &str, config: &Value) -> Vec<String> {
    let mut out = vec![];
    if !MODES.contains(&mode) { out.push(format!("`{mode}` is not a transform mode")); return out; }
    match mode {
        "filter" if config.get("field").and_then(Value::as_str).unwrap_or("").is_empty() => out.push("filter: say `field`".into()),
        "aggregate" if config.get("group_by").and_then(Value::as_str).unwrap_or("").is_empty() => out.push("aggregate: say `group_by`".into()),
        "split" if config.get("by").and_then(Value::as_str) == Some("items") && config.get("n").and_then(Value::as_u64).unwrap_or(0) == 0 => out.push("split: say `n`".into()),
        "limit" if config.get("n").and_then(Value::as_u64).unwrap_or(0) == 0 => out.push("limit: say `n`".into()),
        "rules" if !config.get("fields").map_or(false, Value::is_object) => out.push("rules: say `fields` ({name: {case, max, enum, map, nearest, default}})".into()),
        "template" if config.get("template").and_then(Value::as_str).unwrap_or("").is_empty() => out.push("template: say `template`".into()),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_edit_fields() {
        let out = apply("edit_fields", json!({ "a": 1, "b": 2, "c": 3 }), &json!({ "pick": ["a", "b"], "rename": { "a": "alpha" }, "set": { "d": 4 } })).unwrap();
        assert_eq!(out, json!({ "alpha": 1, "b": 2, "d": 4 }));
    }

    #[test]
    fn mode_filter() {
        let items = json!([{ "k": "a" }, { "k": "b" }, { "k": "a" }]);
        assert_eq!(apply("filter", items.clone(), &json!({ "field": "k", "eq": "a" })).unwrap(), json!([{ "k": "a" }, { "k": "a" }]));
    }

    #[test]
    fn mode_sort() {
        let items = json!([{ "n": 2 }, { "n": 1 }, { "n": 3 }]);
        assert_eq!(apply("sort", items, &json!({ "by": "n" })).unwrap(), json!([{ "n": 1 }, { "n": 2 }, { "n": 3 }]));
    }

    #[test]
    fn mode_aggregate() {
        let items = json!([{ "t": "a", "v": 1 }, { "t": "a", "v": 2 }, { "t": "b", "v": 1 }]);
        let out = apply("aggregate", items, &json!({ "group_by": "t", "op": "sum", "field": "v" })).unwrap();
        assert_eq!(out["a"]["count"], json!(2));
        assert_eq!(out["a"]["sum"], json!(3.0));
    }

    #[test]
    fn mode_split() {
        let md = "# One\nhi\n# Two\nbye";
        assert_eq!(apply("split", json!(md), &json!({ "by": "heading" })).unwrap().as_array().unwrap().len(), 2);
        assert_eq!(apply("split", json!([1, 2, 3, 4]), &json!({ "by": "items", "n": 2 })).unwrap(), json!([[1, 2], [3, 4]]));
    }

    #[test]
    fn mode_limit() {
        assert_eq!(apply("limit", json!([1, 2, 3]), &json!({ "n": 2 })).unwrap(), json!([1, 2]));
    }

    #[test]
    fn mode_dedupe() {
        let items = json!([{ "id": 1 }, { "id": 1 }, { "id": 2 }]);
        assert_eq!(apply("dedupe", items, &json!({ "by": "id" })).unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn mode_template() {
        assert_eq!(apply("template", json!({ "name": "Dan" }), &json!({ "template": "Hello {name}" })).unwrap(), json!("Hello Dan"));
    }

    #[test]
    fn mode_format_json() {
        let raw = apply("format_json", json!({ "a": 1 }), &json!({})).unwrap();
        assert!(raw.as_str().unwrap().contains('\n'));
    }

    #[test]
    fn mode_sanitize() {
        let out = apply("sanitize", json!({ "note": "reach me at a@b.co, sk-abcdefghijklmnopqrstuvwxyz" }), &json!({})).unwrap();
        assert!(!out["note"].as_str().unwrap().contains("@"));
        assert!(!out["note"].as_str().unwrap().contains("sk-"));
        // exercise check rule kinds via shared patterns
        let v = team_check::evaluate(&[team_check::Rule { kind: "pii".into(), ..Default::default() }], out["note"].as_str().unwrap(), &[]);
        assert!(v.pass);
    }

    #[test]
    fn mode_rules_fixes_what_a_model_got_wrong_and_says_what_it_fixed() {
        let cfg = json!({ "fields": { "title": { "case": "upper", "max": 20 }, "motif": { "enum": ["ark", "tablets", "flood", "tower", "plague", "walls"], "nearest": true, "default": "ark", "map": { "lions' den": "walls" } },
          "accent": { "enum": ["gold", "crimson", "azure"], "nearest": true, "default": "gold" } } });
        let o = apply("rules", json!({ "title": "  \"The Burning Bush of Mount Sinai\" ", "motif": "Lions' Den", "accent": "Golden", "extra": 1 }), &cfg).unwrap();
        assert_eq!(o["title"], json!("THE BURNING BUSH OF")); assert_eq!(o["motif"], json!("walls")); assert_eq!(o["accent"], json!("gold")); assert!(o.get("extra").is_none());
        let fx = o["fixes"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>().join(" | "); assert!(fx.contains("trimmed") && fx.contains("map") && fx.contains("contains") && fx.contains("cut to 20"), "{fx}");
        assert_eq!(apply("rules", json!({ "title": "x", "motif": "Tower", "accent": "azur" }), &cfg).unwrap()["accent"], json!("azure"), "one letter off is the nearest");
        assert_eq!(apply("rules", json!({ "title": "x", "motif": "banana", "accent": "q" }), &cfg).unwrap()["motif"], json!("ark"), "no match: the default, said in the fixes");
        let strict = json!({ "fields": { "motif": { "enum": ["ark", "flood"] } } }); assert!(apply("rules", json!({ "motif": "tower" }), &strict).unwrap_err().contains("is not one of ark, flood"));
        assert_eq!(apply("rules", json!({ "a": "x", "b": 2 }), &json!({ "fields": { "a": {} }, "keep_others": true })).unwrap()["b"], json!(2));
        assert!(!check("rules", &json!({})).is_empty() && check("rules", &cfg).is_empty());
    }
}