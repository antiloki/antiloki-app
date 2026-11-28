//! recall-iterative.rs — twin of skills/rag/recall-iterative.mjs. NAMES ARE CONTRACT:
//! `recallIterative`. Budgeted iterative RAG loop: calls a retrieval function up to
//! `max_iterations` times, expanding the query when prior results weren't sufficient, hard-capped
//! on total chars retrieved (`max_tokens * CHARS_PER_TOKEN`).
//!
//! PARITY-DEFERRED: `retrieve_fn` — the JS `retrieve_fn` param is an opaque
//! `async (query, exclude_ids[]) => [{id, text}]` caller-supplied callback (genuinely
//! non-deterministic: it hits a DB/embedding search/network in real callers). This twin ports
//! the ENTIRE deterministic loop control (iteration cap, dedup via `seenIds`, budget-exhaustion
//! cutoff, sufficiency check, broaden-strategy query shrink, `stopped_reason`/`finish` assembly)
//! faithfully, and takes `retrieve_fn` as a Rust closure parameter (`FnMut`) rather than
//! fabricating an internal non-deterministic call — the callback itself IS the parameter, not a
//! hidden network hop, so there is no internal boundary to stub with `Err`. Errors returned by
//! the closure propagate exactly as JS's un-caught `await retrieve_fn(...)` would (no try/catch
//! around it in the source).
//!
//! `context` (JS: default `{}`, unused in the function body) has no Rust parameter — the JS
//! source never reads it, so there is nothing to mirror.

use serde_json::{json, Value};

const CHARS_PER_TOKEN: i64 = 4;

/// JS: `finish(chunks, totalChars, iterations, stopped, last_query)`.
fn finish(chunks: Vec<Value>, total_chars: i64, iterations: i64, stopped: &str, last_query: &str) -> Value {
    json!({
        "chunks": chunks,
        "iterations": iterations,
        "total_chars": total_chars,
        "total_tokens_est": (total_chars as f64 / 4.0).round() as i64,
        "stopped_reason": stopped,
        "final_query": last_query,
    })
}

/// `export default async function recallIterative(input, context = {})`. `retrieve_fn` is the
/// PARITY-DEFERRED caller-supplied callback (see module doc); it's modeled here as `FnMut(&str,
/// &[Value]) -> Result<Value, String>` mirroring `async (query, exclude_ids[]) => [{id, text}]`
/// (JS: throwing/rejecting propagates uncaught — same here via `?`).
pub fn recallIterative(
    input: &Value,
    mut retrieve_fn: impl FnMut(&str, &[Value]) -> Result<Value, String>,
) -> Result<Value, String> {
    let query = input.get("query").and_then(Value::as_str).filter(|s| !s.is_empty());
    let query = match query {
        Some(q) => q,
        None => return Err("recall-iterative: query is required".to_string()),
    };
    let max_iterations = input.get("max_iterations").and_then(Value::as_i64).unwrap_or(3);
    let max_tokens = input.get("max_tokens").and_then(Value::as_i64).unwrap_or(2000);
    let sufficient_chars = input.get("sufficient_chars").and_then(Value::as_i64).unwrap_or(600);
    let expand_strategy = input.get("expand_strategy").and_then(Value::as_str).unwrap_or("broaden").to_string();

    let budget_chars = max_tokens * CHARS_PER_TOKEN;

    let mut current_query = query.to_string();
    let mut chunks: Vec<Value> = Vec::new();
    let mut seen_ids: Vec<Value> = Vec::new();
    let mut total_chars: i64 = 0;
    let mut iter: i64 = 0;
    let mut stopped = "unknown".to_string();

    while iter < max_iterations {
        let fetched = retrieve_fn(&current_query, &seen_ids)?;
        let fetched_arr = fetched.as_array();
        if fetched_arr.map(|a| a.is_empty()).unwrap_or(true) {
            stopped = "no_more_results".to_string();
            break;
        }
        let fetched_arr = fetched_arr.unwrap();

        let mut budget_hit = false;
        for c in fetched_arr {
            let id = c.get("id").cloned().unwrap_or(Value::Null);
            if seen_ids.contains(&id) {
                continue;
            }
            seen_ids.push(id);
            let text = match c.get("text") {
                Some(Value::Null) | None => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
            };
            let text_len = text.chars().count() as i64;
            if total_chars + text_len > budget_chars {
                stopped = "budget_exhausted".to_string();
                budget_hit = true;
                break;
            }
            chunks.push(c.clone());
            total_chars += text_len;
        }
        if budget_hit {
            return Ok(finish(chunks, total_chars, iter + 1, &stopped, &current_query));
        }

        if total_chars >= sufficient_chars {
            stopped = "sufficient".to_string();
            break;
        }

        if expand_strategy == "broaden" {
            let words: Vec<&str> = current_query.split_whitespace().collect();
            if words.len() <= 1 {
                stopped = "cannot_broaden_further".to_string();
                break;
            }
            current_query = words[..words.len() - 1].join(" ");
        }

        iter += 1;
    }

    if iter >= max_iterations {
        stopped = "iterations_exhausted".to_string();
    }
    Ok(finish(chunks, total_chars, iter, &stopped, &current_query))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_query() {
        let e = recallIterative(&json!({}), |_, _| Ok(json!([]))).unwrap_err();
        assert_eq!(e, "recall-iterative: query is required");
    }

    #[test]
    fn stops_on_no_more_results() {
        let out = recallIterative(&json!({ "query": "hello world" }), |_, _| Ok(json!([]))).unwrap();
        assert_eq!(out["stopped_reason"], json!("no_more_results"));
        assert_eq!(out["chunks"], json!([]));
        assert_eq!(out["iterations"], json!(0));
    }

    #[test]
    fn accumulates_across_iterations_until_sufficient() {
        let mut calls = 0;
        let out = recallIterative(
            &json!({ "query": "alpha beta gamma", "sufficient_chars": 10 }),
            |q, exclude| {
                calls += 1;
                assert!(exclude.is_empty() || calls > 1);
                Ok(json!([{ "id": format!("{q}-{calls}"), "text": "12345" }]))
            },
        )
        .unwrap();
        assert_eq!(out["stopped_reason"], json!("sufficient"));
        assert_eq!(out["total_chars"], json!(10));
        assert_eq!(out["iterations"], json!(1)); // broke via `break` at iter==1 (0-indexed second pass)
        assert_eq!(out["chunks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn dedups_by_id_across_iterations() {
        let mut calls = 0;
        let out = recallIterative(&json!({ "query": "one two", "max_iterations": 2 }), |_, _| {
            calls += 1;
            Ok(json!([{ "id": "same", "text": "x" }]))
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(out["chunks"].as_array().unwrap().len(), 1);
        assert_eq!(out["stopped_reason"], json!("cannot_broaden_further"));
    }

    #[test]
    fn stops_on_budget_exhausted_mid_iteration() {
        let out = recallIterative(&json!({ "query": "alpha beta", "max_tokens": 1 }), |_, exclude| {
            if exclude.is_empty() {
                Ok(json!([{ "id": "a", "text": "12" }, { "id": "b", "text": "999" }]))
            } else {
                Ok(json!([]))
            }
        })
        .unwrap();
        // budget_chars = 1*4 = 4; "a" text len 2 fits (total=2), "b" text len 3 -> 2+3=5 > 4 -> budget_exhausted
        assert_eq!(out["stopped_reason"], json!("budget_exhausted"));
        assert_eq!(out["total_chars"], json!(2));
        assert_eq!(out["chunks"].as_array().unwrap().len(), 1);
        assert_eq!(out["chunks"][0]["id"], json!("a"));
        assert_eq!(out["iterations"], json!(1));
    }

    #[test]
    fn broaden_strategy_drops_last_word_each_iteration() {
        let mut queries: Vec<String> = Vec::new();
        let out = recallIterative(&json!({ "query": "alpha beta gamma", "max_iterations": 3 }), |q, _| {
            queries.push(q.to_string());
            Ok(json!([{ "id": queries.len().to_string(), "text": "x" }]))
        })
        .unwrap();
        assert_eq!(queries, vec!["alpha beta gamma", "alpha beta", "alpha"]);
        assert_eq!(out["stopped_reason"], json!("cannot_broaden_further"));
        assert_eq!(out["iterations"], json!(2));
        assert_eq!(out["final_query"], json!("alpha"));
    }

    #[test]
    fn cannot_broaden_further_when_single_word() {
        let out = recallIterative(&json!({ "query": "solo", "max_iterations": 5 }), |_, _| {
            Ok(json!([{ "id": "x", "text": "1" }]))
        })
        .unwrap();
        assert_eq!(out["stopped_reason"], json!("cannot_broaden_further"));
        assert_eq!(out["iterations"], json!(0));
    }

    #[test]
    fn iterations_exhausted_without_broadening() {
        let out = recallIterative(
            &json!({ "query": "alpha beta", "max_iterations": 2, "expand_strategy": "none" }),
            |_, exclude| Ok(json!([{ "id": format!("id{}", exclude.len()), "text": "1" }])),
        )
        .unwrap();
        assert_eq!(out["stopped_reason"], json!("iterations_exhausted"));
        assert_eq!(out["iterations"], json!(2));
        assert_eq!(out["chunks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn null_text_coerces_to_empty_string() {
        let out = recallIterative(&json!({ "query": "q" }), |_, _| Ok(json!([{ "id": "a", "text": Value::Null }])))
            .unwrap();
        assert_eq!(out["total_chars"], json!(0));
        assert_eq!(out["chunks"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn retrieve_fn_error_propagates() {
        let e = recallIterative(&json!({ "query": "q" }), |_, _| Err("boom".to_string())).unwrap_err();
        assert_eq!(e, "boom");
    }

    #[test]
    fn total_tokens_est_rounds_chars_over_four() {
        let out = recallIterative(&json!({ "query": "q", "sufficient_chars": 1 }), |_, _| {
            Ok(json!([{ "id": "a", "text": "123456789" }]))
        })
        .unwrap();
        assert_eq!(out["total_chars"], json!(9));
        assert_eq!(out["total_tokens_est"], json!(2)); // round(9/4) = round(2.25) = 2
    }
}
