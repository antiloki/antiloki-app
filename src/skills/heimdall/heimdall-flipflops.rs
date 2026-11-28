//! heimdall-flipflops.rs — twin of skills/heimdall/heimdall-flipflops.mjs (db READ lane, cross-db
//! ATTACH: judgments.judgment_items / judgments.judgment_cases). NAMES ARE CONTRACT: tokensOf ·
//! isFlipflopPair · heimdallFlipflops (default). The JUDGE-CONSISTENCY metric: within ONE judgment
//! case, two same-side claims about the SAME subject (>=2 shared significant tokens, at least one
//! non-generic) that received OPPOSITE rulings. A labeled heuristic report metric, never a gate.
// QUALITY: faithful clone — naive tokenizer + de-plural, JS Set insertion-order semantics preserved
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

const STOP: &[&str] = &[
    "the", "with", "that", "from", "this", "into", "have", "when", "your", "will", "does", "must",
    "should", "been", "were", "their", "about", "which", "attempt", "fails", "fail",
];
const GENERIC: &[&str] = &[
    "required", "contract", "component", "button", "file", "code", "line", "implement",
    "implemented", "properly", "proper", "element", "elements", "statement", "statements",
];

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
// JS `${v}` for the query-row values here (judgment_id/side TEXT columns — never undefined).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn slice_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

// TRANSITIVE: String(statement ?? '').toLowerCase() tokenized by /[a-z][a-z0-9-]{3,}/g, naive
// de-plural (strip ONE trailing 's'), stopwords stripped, then deduped preserving first-seen
// order — mirrors JS `new Set(...)` iteration order (later code reads that order into output).
pub fn tokensOf(statement: &Value) -> Vec<String> {
    let s = str_qq(Some(statement)).to_lowercase();
    let re = regex::Regex::new(r"[a-z][a-z0-9-]{3,}").unwrap();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for m in re.find_iter(&s) {
        let w = m.as_str();
        let w = w.strip_suffix('s').unwrap_or(w);
        if STOP.contains(&w) { continue; }
        if seen.insert(w.to_string()) { out.push(w.to_string()); }
    }
    out
}

pub fn isFlipflopPair(a_statement: &Value, b_statement: &Value) -> Value {
    let a = tokensOf(a_statement);
    let b = tokensOf(b_statement);
    let b_set: std::collections::HashSet<&str> = b.iter().map(|s| s.as_str()).collect();
    let shared: Vec<String> = a.iter().filter(|t| b_set.contains(t.as_str())).cloned().collect();
    if shared.len() < 2 { return Value::Null; }
    if !shared.iter().any(|t| !GENERIC.contains(&t.as_str())) { return Value::Null; }
    json!(shared)
}

pub fn heimdallFlipflops(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('heimdall-flipflops','db') when no db — the db lane always
    // injects one, so the guard is unreachable and not transcribed (playbook rule 9).
    let card_v = input.get("card_slug");
    let card: Option<String> = if truthy(card_v) { Some(js_string(card_v.unwrap())) } else { None };

    let sql = if card.is_some() {
        "SELECT ji.judgment_id, ji.side, ji.item_id, ji.statement, ji.judge_value \
         FROM judgments.judgment_items ji \
         JOIN judgments.judgment_cases jc ON jc.judgment_id = ji.judgment_id \
         WHERE jc.card_slug = ? \
         ORDER BY ji.judgment_id, ji.side"
    } else {
        "SELECT ji.judgment_id, ji.side, ji.item_id, ji.statement, ji.judge_value \
         FROM judgments.judgment_items ji \
         JOIN judgments.judgment_cases jc ON jc.judgment_id = ji.judgment_id \
         ORDER BY ji.judgment_id, ji.side"
    };
    let params: Vec<Value> = match &card { Some(c) => vec![json!(c)], None => vec![] };
    // JS wraps the query in try/catch → { ok:true, count:0, pairs:[], note:'judgments DB absent' }
    // on failure (e.g. the judgments sibling DB/tables aren't there yet).
    let items: Vec<Value> = match crate::db_json::query_rows(db, sql, &params) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => return Ok(json!({ "ok": true, "count": 0, "pairs": [], "note": "judgments DB absent" })),
    };

    // byCaseSide: Map keyed by `${judgment_id} ${side}`, insertion order preserved (query is
    // already ORDER BY judgment_id, side so groups form as contiguous runs either way).
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<Value>> = HashMap::new();
    for it in &items {
        let key = format!("{} {}", tmpl(it.get("judgment_id")), tmpl(it.get("side")));
        if !groups.contains_key(&key) {
            order.push(key.clone());
            groups.insert(key.clone(), Vec::new());
        }
        groups.get_mut(&key).unwrap().push(it.clone());
    }

    let mut pairs: Vec<Value> = Vec::new();
    for key in &order {
        let group = &groups[key];
        for i in 0..group.len() {
            for j in (i + 1)..group.len() {
                let a = &group[i];
                let b = &group[j];
                if a.get("judge_value") == b.get("judge_value") { continue; } // opposite rulings only
                let a_stmt = a.get("statement").cloned().unwrap_or(Value::Null);
                let b_stmt = b.get("statement").cloned().unwrap_or(Value::Null);
                let shared = isFlipflopPair(&a_stmt, &b_stmt);
                if shared.is_null() { continue; }
                pairs.push(json!({
                    "judgment_id": a.get("judgment_id").cloned().unwrap_or(Value::Null),
                    "a": {
                        "item_id": a.get("item_id").cloned().unwrap_or(Value::Null),
                        "value": a.get("judge_value").cloned().unwrap_or(Value::Null),
                        "statement": slice_chars(&str_qq(Some(&a_stmt)), 160),
                    },
                    "b": {
                        "item_id": b.get("item_id").cloned().unwrap_or(Value::Null),
                        "value": b.get("judge_value").cloned().unwrap_or(Value::Null),
                        "statement": slice_chars(&str_qq(Some(&b_stmt)), 160),
                    },
                    "shared": shared,
                }));
            }
        }
    }

    let count = pairs.len() as i64;
    Ok(json!({ "ok": true, "count": count, "pairs": pairs.into_iter().take(25).collect::<Vec<_>>() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS judgments; \
             CREATE TABLE judgments.judgment_cases (judgment_id TEXT PRIMARY KEY, card_slug TEXT); \
             CREATE TABLE judgments.judgment_items (judgment_id TEXT, side TEXT, item_id TEXT, statement TEXT, judge_value INTEGER);",
        ).unwrap();
        c
    }
    fn case_(c: &Connection, judgment_id: &str, card_slug: &str) {
        c.execute("INSERT INTO judgments.judgment_cases (judgment_id, card_slug) VALUES (?,?)", rusqlite::params![judgment_id, card_slug]).unwrap();
    }
    fn item(c: &Connection, judgment_id: &str, side: &str, item_id: &str, statement: &str, judge_value: i64) {
        c.execute(
            "INSERT INTO judgments.judgment_items (judgment_id, side, item_id, statement, judge_value) VALUES (?,?,?,?,?)",
            rusqlite::params![judgment_id, side, item_id, statement, judge_value],
        ).unwrap();
    }

    #[test]
    fn tokens_of_lowercases_strips_stopwords_deplural_dedupes_in_order() {
        assert_eq!(tokensOf(&json!("The Widget lacks proper ARIA labels")), vec!["widget", "lack", "proper", "aria", "label"]);
        assert_eq!(tokensOf(&Value::Null), Vec::<String>::new());
    }

    #[test]
    fn is_flipflop_pair_needs_two_shared_and_one_non_generic() {
        assert_eq!(
            isFlipflopPair(&json!("widget lacks accessibility labels"), &json!("widget lacks aria label completely")),
            json!(["widget", "lack", "label"])
        );
        assert_eq!(isFlipflopPair(&json!("database connection pool exhausted"), &json!("user login session expired")), Value::Null);
        assert_eq!(isFlipflopPair(&json!("the required contract element"), &json!("this required contract statement")), Value::Null);
    }

    #[test]
    fn finds_flipflop_skips_same_value_weak_overlap_and_all_generic() {
        let c = setup();
        case_(&c, "J-A", "CARD-X");
        item(&c, "J-A", "defender", "d1", "widget lacks accessibility labels", 1);
        item(&c, "J-A", "defender", "d2", "widget lacks aria label completely", 0);
        item(&c, "J-A", "defender", "d3", "database connection pool exhausted today", 1);
        item(&c, "J-A", "prosecutor", "p1", "the required contract element", 1);
        item(&c, "J-A", "prosecutor", "p2", "this required contract statement", 0);

        let r = heimdallFlipflops(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["count"], json!(1));
        assert_eq!(r["pairs"], json!([{
            "judgment_id": "J-A",
            "a": { "item_id": "d1", "value": 1, "statement": "widget lacks accessibility labels" },
            "b": { "item_id": "d2", "value": 0, "statement": "widget lacks aria label completely" },
            "shared": ["widget", "lack", "label"],
        }]));
    }

    #[test]
    fn card_slug_filters_to_that_case_only() {
        let c = setup();
        case_(&c, "J-A", "CARD-X");
        case_(&c, "J-B", "CARD-Y");
        item(&c, "J-A", "defender", "d1", "widget lacks accessibility labels", 1);
        item(&c, "J-A", "defender", "d2", "widget lacks aria label completely", 0);
        item(&c, "J-B", "defender", "e1", "widget lacks accessibility labels", 1);
        item(&c, "J-B", "defender", "e2", "widget lacks aria label completely", 0);

        let r = heimdallFlipflops(&json!({ "card_slug": "CARD-Y" }), &c).unwrap();
        assert_eq!(r["count"], json!(1));
        assert_eq!(r["pairs"][0]["judgment_id"], json!("J-B"));
    }

    #[test]
    fn caps_pairs_at_25_but_count_is_the_full_total() {
        let c = setup();
        case_(&c, "J-C", "CARD-Z");
        for i in 0..6 { item(&c, "J-C", "defender", &format!("a{i}"), "widget lacks accessibility labels", 1); }
        for i in 0..6 { item(&c, "J-C", "defender", &format!("b{i}"), "widget lacks aria label completely", 0); }

        let r = heimdallFlipflops(&json!({}), &c).unwrap();
        assert_eq!(r["count"], json!(36)); // 6 * 6 opposite-value cross pairs, all sharing the subject
        assert_eq!(r["pairs"].as_array().unwrap().len(), 25);
    }

    #[test]
    fn judgments_db_absent_is_reported_not_erred() {
        let c = Connection::open_in_memory().unwrap(); // no ATTACH judgments at all
        let r = heimdallFlipflops(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "count": 0, "pairs": [], "note": "judgments DB absent" }));
    }
}
