//! WHAT THE SAME TOKENS WOULD COST AT OPENROUTER (operator 2026-11-21: "the value should compare exactly the AIs used with their price on OpenRouter"): OpenRouter's public model list gives a price per token in and out for every model. Read once a day and kept in `openrouter-prices.json` beside the other machine-wide files;
//! a small baked list answers when the network does not. A model whose price is not known gets `null`, never a guess. Pure matching, tested.
use serde_json::{json, Map, Value};

/// USD per 1M tokens (in, out) for the models antiloki routes to most; the fallback when the list cannot be read. From OpenRouter's list of 2026-11-21.
pub const BAKED: &[(&str, f64, f64)] = &[
    ("anthropic/claude-opus-5.5", 4.0, 20.0), ("anthropic/claude-sonnet-5.5", 2.0, 10.0), ("anthropic/claude-haiku-5.5", 0.1, 0.5), ("anthropic/claude-opus-4.8", 5.0, 25.0), ("anthropic/claude-sonnet-4.6", 3.0, 15.0),
    ("openai/gpt-5.5", 5.0, 30.0), ("openai/gpt-5.6-sol", 4.0, 20.0), ("google/gemini-3.5-flash", 1.5, 9.0), ("google/gemini-3.1-pro-preview", 2.0, 12.0), ("x-ai/grok-4.7", 2.0, 6.0), ("deepseek/deepseek-v4-pro", 0.9, 1.5), ("deepseek/deepseek-v4-flash", 0.09, 0.18),
];
/// A model id as the calls record it (`claude-opus-5-5`, `anthropic/claude-opus-5.5`, `gpt-5.5-high`) reduced to what can be compared: no provider prefix, lower case, dots as dashes. Pure.
pub fn norm(id: &str) -> String { id.rsplit('/').next().unwrap_or(id).to_ascii_lowercase().replace('.', "-").replace('_', "-") }
/// The price of a model by its id: the same normalised id, else the longest listed id the model's id starts with (`claude-opus-5-5-20261101` → `claude-opus-5-5`). `(in, out)` per 1M. Pure.
pub fn price_for(model: &str, table: &[(String, f64, f64)]) -> Option<(f64, f64)> {
    let m = norm(model); if m.is_empty() || m == "?" { return None }
    if let Some(r) = table.iter().find(|r| norm(&r.0) == m) { return Some((r.1, r.2)); }
    table.iter().filter(|r| { let n = norm(&r.0); m.starts_with(&n) && m.len() > n.len() && !m[n.len()..].starts_with(|c: char| c.is_ascii_digit()) }).max_by_key(|r| norm(&r.0).len()).map(|r| (r.1, r.2))
}
/// The cost of tokens at a price per 1M. Pure.
pub fn cost_at(price: (f64, f64), tokens_in: f64, tokens_out: f64) -> f64 { (tokens_in * price.0 + tokens_out * price.1) / 1_000_000.0 }
/// The same with the prompt cache: reads at 10% and writes at 125% of the input price (the providers' published rates). Pure.
pub fn cost_cached(price: (f64, f64), inp: f64, out: f64, cache_read: f64, cache_write: f64) -> f64 { (inp * price.0 + out * price.1 + cache_read * price.0 * 0.1 + cache_write * price.0 * 1.25) / 1_000_000.0 }
/// OpenRouter's answer (`{data:[{id, pricing:{prompt, completion}}]}`) as rows of (id, in, out) per 1M; free or unpriced models left out. Pure.
pub fn parse_list(v: &Value) -> Vec<(String, f64, f64)> {
    v["data"].as_array().into_iter().flatten().filter_map(|m| { let id = m["id"].as_str()?; if id.contains(':') { return None } let p = |k: &str| m["pricing"][k].as_str().and_then(|x| x.parse::<f64>().ok()).or_else(|| m["pricing"][k].as_f64()); let (i, o) = (p("prompt")?, p("completion")?); if i <= 0.0 && o <= 0.0 { return None } Some((id.to_string(), i * 1e6, o * 1e6)) }).collect()
}
fn file() -> std::path::PathBuf { antiloki_rs::cockpit__helpers_webapps::store_path().with_file_name("openrouter-prices.json") }
/// The table: the file when it is under a day old, else a fresh read of OpenRouter (written back), else the old file, else the baked list. Blocking (a network read); call from `spawn_blocking`.
pub fn table() -> Vec<(String, f64, f64)> {
    let p = file(); let now = crate::cli::now_ms() as i64;
    let saved = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let rows_of = |v: &Value| -> Vec<(String, f64, f64)> { v["rows"].as_array().into_iter().flatten().filter_map(|r| Some((r[0].as_str()?.to_string(), r[1].as_f64()?, r[2].as_f64()?))).collect() };
    if let Some(v) = &saved { if now - v["t_ms"].as_i64().unwrap_or(0) < 86_400_000 { let r = rows_of(v); if !r.is_empty() { return r; } } }
    let fresh = ureq::get("https://openrouter.ai/api/v1/models").timeout(std::time::Duration::from_secs(8)).call().ok().and_then(|r| r.into_json::<Value>().ok()).map(|v| parse_list(&v)).filter(|r| !r.is_empty());
    if let Some(r) = fresh { let _ = std::fs::write(&p, json!({ "t_ms": now, "rows": r.iter().map(|x| json!([x.0, x.1, x.2])).collect::<Vec<_>>() }).to_string()); return r; }
    if let Some(v) = &saved { let r = rows_of(v); if !r.is_empty() { return r; } }
    BAKED.iter().map(|b| (b.0.to_string(), b.1, b.2)).collect()
}
/// A fuel `models` row with what its tokens would cost at OpenRouter: `or_in`, `or_out` (per 1M) and `or_cost`, or `null`s. Pure.
pub fn priced(row: &Value, table: &[(String, f64, f64)]) -> Value {
    let mut r = row.clone(); let o = r.as_object_mut().map(|o| o as &mut Map<String, Value>);
    if let Some(o) = o { match price_for(row["model"].as_str().unwrap_or(""), table) { Some(pr) => { o.insert("or_in".into(), json!(pr.0)); o.insert("or_out".into(), json!(pr.1)); o.insert("or_cost".into(), json!(cost_cached(pr, row["tokens_in"].as_f64().unwrap_or(0.0), row["tokens_out"].as_f64().unwrap_or(0.0), row["cache_read"].as_f64().unwrap_or(0.0), row["cache_write"].as_f64().unwrap_or(0.0)))); } None => { o.insert("or_in".into(), Value::Null); o.insert("or_out".into(), Value::Null); o.insert("or_cost".into(), Value::Null); } } }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t() -> Vec<(String, f64, f64)> { BAKED.iter().map(|b| (b.0.to_string(), b.1, b.2)).collect() }
    #[test] fn a_model_id_finds_its_price_by_the_same_name_or_its_longest_listed_prefix() {
        let t = t(); assert_eq!(price_for("claude-opus-5-5", &t), Some((4.0, 20.0))); assert_eq!(price_for("anthropic/claude-sonnet-5.5", &t), Some((2.0, 10.0))); assert_eq!(price_for("claude-opus-5-5-20261101", &t), Some((4.0, 20.0)));
        assert_eq!(price_for("gpt-5.5-high", &t), Some((5.0, 30.0))); assert_eq!(price_for("claude-opus-5", &t), None, "a shorter id is not a longer model"); assert_eq!(price_for("mystery-model", &t), None); assert_eq!(price_for("?", &t), None);
        assert_eq!(price_for("gemini-3.5-flash", &t), Some((1.5, 9.0)), "the version digit is not swallowed: gpt-5.55 is not gpt-5.5"); assert_eq!(price_for("gpt-5.55", &t), None);
    }
    #[test] fn the_cache_is_priced_at_its_own_rates() { assert!((cost_cached((4.0, 20.0), 0.0, 0.0, 1_000_000.0, 1_000_000.0) - (0.4 + 5.0)).abs() < 1e-9); let t = t(); let r = priced(&json!({ "model": "claude-opus-5-5", "tokens_in": 0, "tokens_out": 0, "cache_read": 1_000_000, "cache_write": 0 }), &t); assert!((r["or_cost"].as_f64().unwrap() - 0.4).abs() < 1e-9); }
    #[test] fn tokens_cost_what_the_list_says() { assert!((cost_at((4.0, 20.0), 1_000_000.0, 500_000.0) - 14.0).abs() < 1e-9); }
    #[test] fn the_list_is_read_per_million_and_free_or_unpriced_models_are_left_out() {
        let v = json!({ "data": [ { "id": "a/m1", "pricing": { "prompt": "0.000002", "completion": "0.00001" } }, { "id": "a/m1:batch", "pricing": { "prompt": "0.000001", "completion": "0.000005" } }, { "id": "a/free", "pricing": { "prompt": "0", "completion": "0" } }, { "id": "a/odd", "pricing": {} } ] });
        let r = parse_list(&v); assert_eq!(r.len(), 1); assert_eq!(r[0].0, "a/m1"); assert!((r[0].1 - 2.0).abs() < 1e-9 && (r[0].2 - 10.0).abs() < 1e-9);
    }
    #[test] fn a_row_is_priced_or_says_null() {
        let t = t(); let a = priced(&json!({ "model": "claude-haiku-5-5", "tokens_in": 1_000_000, "tokens_out": 1_000_000 }), &t); assert!((a["or_cost"].as_f64().unwrap() - 0.6).abs() < 1e-9); let b = priced(&json!({ "model": "zzz", "tokens_in": 5 }), &t); assert!(b["or_cost"].is_null());
    }
}
