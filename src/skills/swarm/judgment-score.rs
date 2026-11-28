//! judgment-score.rs — twin of skills/swarm/judgment-score.mjs (no-db, pure). NAMES ARE CONTRACT:
//! extractJsonBlock · parseAccuracy · parseVerdicts (transitive — returns a Map) · scoreSide ·
//! scoreJudgment · judgmentScore (default). The deterministic verdict counter for the per-file review.
// QUALITY: 84/100 — grounding floor + majority sufficiency; the model classifies, THIS code counts.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

// ── JS coercion helpers ────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    // String(v ?? '')
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String {
    // String(x) for a defined x
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn js_string_full(v: Option<&Value>) -> String {
    // String(x) where x may be undefined
    match v {
        None => "undefined".into(),
        Some(x) => js_string(x),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    // `${v}`
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
} // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 {
    // Number(v)
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}
// Standard JS truthiness (used for `if (!obj)`).
fn std_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS number → JSON.stringify rendering: integral values print without a decimal point.
fn js_num(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 {
        json!(n as i64)
    } else {
        json!(n)
    }
}
// (n).toFixed(2)
fn to_fixed2(n: f64) -> String {
    format!("{:.2}", n)
}

// The judge's verdict truthiness (NOT standard JS truthiness):
// v === true || v === 1 || v === '1' || (string in [true,yes,pass,t,y]).
fn verdict_truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(true)) => true,
        Some(Value::Number(n)) => n.as_f64() == Some(1.0),
        Some(Value::String(s)) => {
            if s == "1" {
                return true;
            }
            matches!(s.trim().to_lowercase().as_str(), "true" | "yes" | "pass" | "t" | "y")
        }
        _ => false,
    }
}

fn fenced_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)```(?:json)?\s*([\s\S]*?)```").unwrap())
}
fn obj_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\{[\s\S]*\}").unwrap())
}
fn arr_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\[[\s\S]*\]").unwrap())
}

// Pull the first fenced ```json block (or any fenced block, or a bare {...}/[...]).
pub fn extractJsonBlock(text: &Value) -> Value {
    if matches!(text, Value::Object(_) | Value::Array(_)) {
        return text.clone(); // already parsed
    }
    let s = str_qq(Some(text)); // String(text ?? '')
    let mut candidates: Vec<String> = Vec::new();
    if let Some(c) = fenced_re().captures(&s) {
        candidates.push(c[1].to_string()); // a fenced block wins
    }
    candidates.push(s.clone()); // the whole reply
    if let Some(m) = obj_re().find(&s) {
        candidates.push(m.as_str().to_string()); // an object embedded in prose
    }
    if let Some(m) = arr_re().find(&s) {
        candidates.push(m.as_str().to_string()); // an array embedded in prose
    }
    for c in &candidates {
        if let Ok(v) = serde_json::from_str::<Value>(c.trim()) {
            return v;
        }
    }
    Value::Null
}

// Parse the judge's per-claim accuracy grade into [0,1], or null when absent/unparseable.
pub fn parseAccuracy(v: &Value) -> Value {
    if v.is_null() {
        return Value::Null; // v == null
    }
    let mut n = match v {
        Value::String(s) => {
            let stripped = s.replacen('%', "", 1); // v.replace('%','') — first only
            let t = stripped.trim();
            if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) }
        }
        other => js_number(Some(other)),
    };
    if !n.is_finite() {
        return Value::Null;
    }
    if n > 1.0 {
        n /= 100.0; // a 0–100 grade → fraction
    }
    let clamped = if n < 0.0 { 0.0 } else if n > 1.0 { 1.0 } else { n };
    js_num(clamped)
}

struct VEntry {
    value: bool,
    accuracy: Value,
    why: Value,
}

// The Map-building core shared by the transitive parseVerdicts and scoreSide. Preserves
// insertion order; duplicate ids overwrite (last-set-wins, like a JS Map).
fn parse_verdicts_entries(reply: &Value) -> Vec<(String, VEntry)> {
    let obj = extractJsonBlock(reply);
    let mut out: Vec<(String, VEntry)> = Vec::new();
    if !std_truthy(Some(&obj)) {
        return out; // if (!obj) return map;
    }
    let list: Option<&Vec<Value>> = if obj.is_array() {
        obj.as_array()
    } else if obj.get("verdicts").map_or(false, |v| v.is_array()) {
        obj.get("verdicts").and_then(|v| v.as_array())
    } else if obj.get("items").map_or(false, |v| v.is_array()) {
        obj.get("items").and_then(|v| v.as_array())
    } else {
        None
    };
    if let Some(list) = list {
        for v in list {
            let id = match nz(v.get("item_id")).or_else(|| nz(v.get("id"))) {
                Some(x) => x,
                None => continue, // if (id == null) continue
            };
            let key = js_string(id); // String(id)
            let value = verdict_truthy(
                nz(v.get("value"))
                    .or_else(|| nz(v.get("verdict")))
                    .or_else(|| nz(v.get("true"))),
            );
            let acc_src = nz(v.get("accuracy"))
                .or_else(|| nz(v.get("grade")))
                .or_else(|| nz(v.get("score")))
                .cloned()
                .unwrap_or(Value::Null);
            let accuracy = parseAccuracy(&acc_src);
            let why = nz(v.get("why"))
                .or_else(|| nz(v.get("reason")))
                .cloned()
                .unwrap_or(json!(""));
            out.push((key, VEntry { value, accuracy, why }));
        }
    } else if let Some(o) = obj.as_object() {
        // bare { item_id: true, ... } map form
        for (id, v) in o {
            if matches!(v, Value::Bool(_) | Value::Number(_) | Value::String(_)) {
                out.push((
                    id.clone(),
                    VEntry { value: verdict_truthy(Some(v)), accuracy: Value::Null, why: json!("") },
                ));
            }
        }
    }
    out
}

// JS Map.get: the last value set for a key.
fn lookup<'a>(entries: &'a [(String, VEntry)], key: &str) -> Option<&'a VEntry> {
    entries.iter().rev().find(|(k, _)| k == key).map(|(_, e)| e)
}

// TRANSITIVE (returns a Map in JS): materialize the id → {value, accuracy, why} map as an object.
pub fn parseVerdicts(reply: &Value) -> Value {
    let entries = parse_verdicts_entries(reply);
    let mut m = Map::new();
    for (k, e) in &entries {
        m.insert(
            k.clone(),
            json!({ "value": e.value, "accuracy": e.accuracy, "why": e.why }),
        ); // preserve_order: existing key keeps its position (Map overwrite)
    }
    Value::Object(m)
}

struct SideResult {
    items: Value,
    trues: i64,
    total: i64,
    rate_f: f64,
    rate_v: Value,
    accuracy: Value,
}

fn score_side_raw(list: &Value, reply: &Value) -> SideResult {
    let entries = parse_verdicts_entries(reply);
    let empty: Vec<Value> = Vec::new();
    let list_arr = list.as_array().unwrap_or(&empty); // list ?? []
    let mut items: Vec<Value> = Vec::new();
    let mut trues: i64 = 0;
    let mut acc_sum = 0.0f64;
    let mut acc_count: i64 = 0;
    for it in list_arr {
        let ve = lookup(&entries, &js_string_full(it.get("item_id"))); // verdicts.get(String(it.item_id))
        let judged = ve.map(|e| e.value).unwrap_or(false); // unaddressed → false (unproven)
        let grounded = !matches!(it.get("grounded"), Some(Value::Bool(false))); // it.grounded !== false
        let value = grounded && judged;
        let accuracy: Value = if grounded {
            match ve {
                Some(e) if !e.accuracy.is_null() => e.accuracy.clone(), // v?.accuracy ?? null
                _ => Value::Null,
            }
        } else {
            json!(0) // ungrounded → forced 0 by the floor
        };
        let why: Value = match ve {
            Some(e) if !e.why.is_null() => e.why.clone(), // v?.why ?? ''
            _ => json!(""),
        };
        if value {
            trues += 1;
        }
        if !accuracy.is_null() {
            acc_sum += accuracy.as_f64().unwrap_or(0.0);
            acc_count += 1;
        }
        let mut o = Map::new();
        if let Some(id) = it.get("item_id") {
            o.insert("item_id".into(), id.clone()); // undefined item_id → key omitted
        }
        o.insert("statement".into(), nz(it.get("statement")).cloned().unwrap_or(json!(""))); // it.statement ?? ''
        o.insert(
            "links".into(),
            it.get("links").filter(|v| v.is_array()).cloned().unwrap_or(json!([])),
        );
        o.insert("grounded".into(), json!(grounded));
        o.insert("judge_value".into(), json!(judged));
        o.insert("value".into(), json!(value));
        o.insert("accuracy".into(), accuracy);
        o.insert("why".into(), why);
        items.push(Value::Object(o));
    }
    let total = items.len() as i64;
    let (rate_f, rate_v) = if total == 0 {
        (0.0, json!(0))
    } else {
        let r = trues as f64 / total as f64;
        (r, js_num(r))
    };
    let accuracy = if acc_count > 0 {
        js_num(acc_sum / acc_count as f64)
    } else {
        Value::Null
    };
    SideResult { items: Value::Array(items), trues, total, rate_f, rate_v, accuracy }
}

pub fn scoreSide(list: &Value, reply: &Value) -> Value {
    let r = score_side_raw(list, reply);
    json!({ "items": r.items, "trues": r.trues, "total": r.total, "rate": r.rate_v, "accuracy": r.accuracy })
}

fn score_judgment_impl(input: &Value) -> Result<Value, String> {
    let dl = input.get("defender_list").filter(|v| v.is_array());
    let pl = input.get("prosecutor_list").filter(|v| v.is_array());
    let (dl, pl) = match (dl, pl) {
        (Some(d), Some(p)) => (d, p),
        _ => return Err("judgment-score: defender_list + prosecutor_list is required".into()),
    };
    let dl_len = dl.as_array().map(|a| a.len()).unwrap_or(0);
    let pl_len = pl.as_array().map(|a| a.len()).unwrap_or(0);

    // thresholds = { ...DEFAULTS, ...(input?.thresholds ?? {}) }
    let mut thresholds = Map::new();
    thresholds.insert("def_min".into(), json!(0.8));
    thresholds.insert("pros_max".into(), json!(0.2));
    if let Some(t) = input.get("thresholds").and_then(|v| v.as_object()) {
        for (k, v) in t {
            thresholds.insert(k.clone(), v.clone());
        }
    }

    let mut reasons: Vec<Value> = Vec::new();

    if dl_len != pl_len {
        reasons.push(json!(format!(
            "list length mismatch: defender={} prosecutor={} \u{2014} both sides must submit the same number of items",
            dl_len, pl_len
        )));
        let n = std::cmp::max(dl_len, pl_len) as i64;
        return Ok(json!({
            "verdict": "invalid", "valid": false, "n": n,
            "defender": { "trues": 0, "rate": 0, "accuracy": null, "items": [] },
            "prosecutor": { "trues": 0, "rate": 0, "accuracy": null, "items": [] },
            "thresholds": Value::Object(thresholds), "reasons": Value::Array(reasons),
        }));
    }
    if dl_len == 0 {
        reasons.push(json!("empty lists \u{2014} nothing to judge"));
        return Ok(json!({
            "verdict": "invalid", "valid": false, "n": 0,
            "defender": { "trues": 0, "rate": 0, "accuracy": null, "items": [] },
            "prosecutor": { "trues": 0, "rate": 0, "accuracy": null, "items": [] },
            "thresholds": Value::Object(thresholds), "reasons": Value::Array(reasons),
        }));
    }

    let defender = score_side_raw(dl, input.get("judge_defender").unwrap_or(&Value::Null));
    let prosecutor = score_side_raw(pl, input.get("judge_prosecutor").unwrap_or(&Value::Null));

    let def_min = js_number(thresholds.get("def_min"));
    let pros_max = js_number(thresholds.get("pros_max"));

    let prosecutor_ok = prosecutor.rate_f <= pros_max;
    let defender_strict_ok = defender.rate_f >= def_min;
    let defender_majority_ok = prosecutor.trues == 0 && defender.rate_f > 0.5;
    let defender_ok = defender_strict_ok || defender_majority_ok;
    let verdict = if prosecutor_ok && defender_ok { "pass" } else { "fail" };

    if !prosecutor_ok {
        reasons.push(json!(format!(
            "prosecutor true-rate {} > {} \u{2014} a grounded defect stands ({}/{})",
            to_fixed2(prosecutor.rate_f),
            tmpl(thresholds.get("pros_max")),
            prosecutor.trues,
            prosecutor.total
        )));
    }
    if !defender_ok {
        reasons.push(json!(format!(
            "defender true-rate {} < {} and no majority with zero defects \u{2014} sufficiency not established ({}/{})",
            to_fixed2(defender.rate_f),
            tmpl(thresholds.get("def_min")),
            defender.trues,
            defender.total
        )));
    } else if defender_majority_ok && !defender_strict_ok {
        reasons.push(json!(format!(
            "sufficiency by majority: zero grounded defects + defender {}/{} (>50%) \u{2014} clean code isn't failed on one soft sufficiency claim",
            defender.trues, defender.total
        )));
    }
    if verdict == "pass" {
        reasons.push(json!(format!(
            "pass: defender {}/{}, prosecutor {}/{}",
            defender.trues, defender.total, prosecutor.trues, prosecutor.total
        )));
    }

    Ok(json!({
        "verdict": verdict, "valid": true, "n": dl_len as i64,
        "defender": { "trues": defender.trues, "rate": defender.rate_v, "accuracy": defender.accuracy, "items": defender.items },
        "prosecutor": { "trues": prosecutor.trues, "rate": prosecutor.rate_v, "accuracy": prosecutor.accuracy, "items": prosecutor.items },
        "thresholds": Value::Object(thresholds), "reasons": Value::Array(reasons),
    }))
}

pub fn scoreJudgment(input: &Value) -> Value {
    // Success-path only in fixtures; the guard-throw (missing lists) is not exercised.
    score_judgment_impl(input).unwrap_or(Value::Null)
}

pub fn judgmentScore(input: &Value, _db: &Connection) -> Result<Value, String> {
    score_judgment_impl(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jb(verdicts: Value) -> Value {
        json!(format!("```json\n{}\n```", serde_json::to_string(&json!({ "verdicts": verdicts })).unwrap()))
    }
    fn list(ids: &[&str]) -> Value {
        json!(ids.iter().map(|id| json!({ "item_id": id, "statement": format!("{} claim", id) })).collect::<Vec<_>>())
    }

    #[test]
    fn extract_json_block() {
        assert_eq!(extractJsonBlock(&json!("noise\n```json\n{\"a\":1}\n```\nmore")), json!({ "a": 1 }));
        assert_eq!(extractJsonBlock(&json!("```\n{\"a\":2}\n```")), json!({ "a": 2 }));
        assert_eq!(extractJsonBlock(&json!("the answer is {\"a\":3} ok")), json!({ "a": 3 }));
        assert_eq!(extractJsonBlock(&json!("no json here")), Value::Null);
        assert_eq!(extractJsonBlock(&json!({ "already": "parsed" })), json!({ "already": "parsed" }));
        assert_eq!(extractJsonBlock(&json!("42")), json!(42)); // parses to a bare number
    }

    #[test]
    fn parse_accuracy() {
        assert_eq!(parseAccuracy(&json!(0.75)), json!(0.75));
        assert_eq!(parseAccuracy(&json!(85)), json!(0.85));
        assert_eq!(parseAccuracy(&json!("90%")), json!(0.9));
        assert_eq!(parseAccuracy(&json!(150)), json!(1)); // clamp above 100 → integer render
        assert_eq!(parseAccuracy(&json!(-0.5)), json!(0)); // clamp low → integer render
        assert_eq!(parseAccuracy(&Value::Null), Value::Null);
        assert_eq!(parseAccuracy(&json!("n/a")), Value::Null);
    }

    #[test]
    fn parse_verdicts_transitive() {
        let m = parseVerdicts(&jb(json!([{ "item_id": "d1", "value": true, "accuracy": 0.9 }, { "item_id": "d2", "value": false }])));
        assert_eq!(m["d1"]["value"], json!(true));
        assert_eq!(m["d1"]["accuracy"], json!(0.9));
        assert_eq!(m["d2"]["accuracy"], Value::Null);
        // bare-map form
        let bm = parseVerdicts(&json!("{\"d1\": true, \"d2\": false}"));
        assert_eq!(bm["d1"]["value"], json!(true));
        assert_eq!(bm["d2"]["value"], json!(false));
    }

    #[test]
    fn score_side() {
        let l = json!([{ "item_id": "a" }, { "item_id": "b" }, { "item_id": "c", "grounded": false }]);
        let r = scoreSide(&l, &jb(json!([{ "item_id": "a", "value": true }, { "item_id": "c", "value": true }])));
        assert_eq!(r["trues"], json!(1)); // only a (b unaddressed, c ungrounded)
        let items = r["items"].as_array().unwrap();
        assert_eq!(items.iter().find(|i| i["item_id"] == json!("c")).unwrap()["value"], json!(false));
        assert_eq!(items.iter().find(|i| i["item_id"] == json!("b")).unwrap()["value"], json!(false));
        assert_eq!(r["rate"], json!(0.3333333333333333));
        // item "c" is ungrounded -> its per-item accuracy is FORCED to 0 (not null) by the floor;
        // 0 is not null, so it DOES count toward the graded-items mean (a/b stay null, excluded) —
        // graded=[c], sum=0, mean=0/1=0. Not null: only a fully-ungraded set (0 graded items)
        // yields a null aggregate.
        assert_eq!(r["accuracy"], json!(0));
    }

    #[test]
    fn score_side_accuracy_mean() {
        let l = json!([{ "item_id": "a" }, { "item_id": "b" }, { "item_id": "c", "grounded": false }]);
        let r = scoreSide(&l, &jb(json!([
            { "item_id": "a", "value": true, "accuracy": 1.0 },
            { "item_id": "b", "value": true, "accuracy": 0.5 },
            { "item_id": "c", "value": true, "accuracy": 0.9 }
        ])));
        let items = r["items"].as_array().unwrap();
        assert_eq!(items.iter().find(|i| i["item_id"] == json!("c")).unwrap()["accuracy"], json!(0));
        assert_eq!(r["accuracy"], json!(0.5)); // (1 + 0.5 + 0) / 3
    }

    #[test]
    fn score_judgment_pass_and_fail() {
        let db = Connection::open_in_memory().unwrap();
        // healthy pass
        let r = judgmentScore(&json!({
            "defender_list": list(&["d1", "d2"]),
            "prosecutor_list": list(&["p1", "p2"]),
            "judge_defender": jb(json!([{ "item_id": "d1", "value": true }, { "item_id": "d2", "value": true }])),
            "judge_prosecutor": jb(json!([{ "item_id": "p1", "value": false }, { "item_id": "p2", "value": false }])),
        }), &db).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        assert_eq!(r["defender"]["rate"], json!(1));
        assert_eq!(r["prosecutor"]["rate"], json!(0));
        assert_eq!(r["thresholds"], json!({ "def_min": 0.8, "pros_max": 0.2 }));

        // r41 majority pass
        let maj = scoreJudgment(&json!({
            "defender_list": list(&["d1", "d2", "d3"]),
            "prosecutor_list": list(&["p1", "p2", "p3"]),
            "judge_defender": jb(json!([{ "item_id": "d1", "value": true }, { "item_id": "d2", "value": true }, { "item_id": "d3", "value": false }])),
            "judge_prosecutor": jb(json!([{ "item_id": "p1", "value": false }, { "item_id": "p2", "value": false }, { "item_id": "p3", "value": false }])),
        }));
        assert_eq!(maj["verdict"], json!("pass"));
        assert!(maj["reasons"].as_array().unwrap().iter().any(|r| r.as_str().unwrap().contains("sufficiency by majority")));

        // prosecutor defect → fail
        let f = scoreJudgment(&json!({
            "defender_list": list(&["d1", "d2"]),
            "prosecutor_list": list(&["p1", "p2"]),
            "judge_defender": jb(json!([{ "item_id": "d1", "value": true }, { "item_id": "d2", "value": true }])),
            "judge_prosecutor": jb(json!([{ "item_id": "p1", "value": true }, { "item_id": "p2", "value": false }])),
        }));
        assert_eq!(f["verdict"], json!("fail"));
        assert!(f["reasons"].as_array().unwrap().iter().any(|r| r.as_str().unwrap().contains("prosecutor true-rate")));
    }

    #[test]
    fn score_judgment_invalid_and_thresholds() {
        // unequal lengths
        let inv = scoreJudgment(&json!({ "defender_list": list(&["d1", "d2"]), "prosecutor_list": list(&["p1"]), "judge_defender": "```json\n{\"verdicts\":[]}\n```", "judge_prosecutor": "```json\n{\"verdicts\":[]}\n```" }));
        assert_eq!(inv["verdict"], json!("invalid"));
        assert_eq!(inv["valid"], json!(false));
        assert_eq!(inv["n"], json!(2));
        // strict def_min via integer 1
        let strict = scoreJudgment(&json!({
            "defender_list": list(&["d1", "d2"]),
            "prosecutor_list": list(&["p1", "p2"]),
            "judge_defender": jb(json!([{ "item_id": "d1", "value": true }, { "item_id": "d2", "value": false }])),
            "judge_prosecutor": jb(json!([{ "item_id": "p1", "value": false }, { "item_id": "p2", "value": false }])),
            "thresholds": { "def_min": 1 },
        }));
        assert_eq!(strict["verdict"], json!("fail"));
        assert_eq!(strict["thresholds"]["def_min"], json!(1));
    }
}
