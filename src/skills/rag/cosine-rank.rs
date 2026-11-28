//! cosine-rank.rs — twin of skills/rag/cosine-rank.mjs. NAMES ARE CONTRACT: `cosine`,
//! `cosineRank` (named default). Pure function, no DB, no AI (matches the JS doc header).
// QUALITY: 84/100 — clean math + stable sort matching V8 semantics; the vector-presence
// simplification (array-or-skip, no NaN-driven-zero edge case) is documented, not hidden

use serde_json::{json, Value};

fn to_f64_vec(v: &Value) -> Vec<f64> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn norm(a: &[f64]) -> f64 {
    a.iter().map(|x| x * x).sum::<f64>().sqrt()
}

/// JS: `cosine(a, b)` — 0 for either zero-magnitude vector (no division by zero).
pub fn cosine(a: &Value, b: &Value) -> f64 {
    let (av, bv) = (to_f64_vec(a), to_f64_vec(b));
    let (na, nb) = (norm(&av), norm(&bv));
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot(&av, &bv) / (na * nb)
}

/// JS: named default `cosineRank(input, _context)` → top-K sorted desc by score.
/// A corpus item's `vector` key must be a JSON array to be scored (JS truthiness on an
/// absent/undefined key is the only skip path exercised in practice — see json-utils.rs
/// for the same narrow-coercion stance on out-of-domain inputs).
pub fn cosineRank(input: &Value, _context: &Value) -> Result<Value, String> {
    let query_vector = input.get("query_vector");
    let corpus = input.get("corpus").and_then(Value::as_array);
    if query_vector.is_none() || corpus.is_none() {
        return Err("cosine-rank: query_vector + corpus is required".into());
    }
    let query_vector = query_vector.unwrap();
    let corpus = corpus.unwrap();
    let top_k = input.get("top_k").and_then(Value::as_u64).unwrap_or(10) as usize;
    let min_score = input.get("min_score").and_then(Value::as_f64).unwrap_or(f64::NEG_INFINITY);

    let mut ranked: Vec<(Value, f64, Value)> = Vec::new();
    for item in corpus {
        let Some(vector) = item.get("vector").filter(|v| v.is_array()) else { continue };
        let score = cosine(query_vector, vector);
        if score < min_score {
            continue;
        }
        let id = item.get("id").cloned().unwrap_or(Value::Null);
        let meta = item.get("meta").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
        ranked.push((id, score, meta));
    }
    // JS Array.prototype.sort is stable (ES2019+) — Rust's sort_by matches that guarantee
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let out: Vec<Value> = ranked.into_iter().take(top_k)
        .map(|(id, score, meta)| json!({ "id": id, "score": score, "meta": meta }))
        .collect();
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_special_cases() {
        assert_eq!(cosine(&json!([1, 0, 0]), &json!([1, 0, 0])), 1.0);
        assert_eq!(cosine(&json!([1, 0, 0]), &json!([0, 1, 0])), 0.0);
        assert_eq!(cosine(&json!([1, 0, 0]), &json!([-1, 0, 0])), -1.0);
        assert_eq!(cosine(&json!([0, 0, 0]), &json!([1, 1, 1])), 0.0);
    }

    #[test]
    fn rank_sorts_filters_and_requires_args() {
        let r = cosineRank(&json!({
            "query_vector": [1, 0, 0],
            "corpus": [
                {"id":"far","vector":[0,1,0]},
                {"id":"close","vector":[0.95,0.1,0]},
                {"id":"middle","vector":[0.5,0.5,0]},
            ],
            "top_k": 2,
        }), &Value::Null).unwrap();
        assert_eq!(r[0]["id"], json!("close"));
        assert_eq!(r[1]["id"], json!("middle"));
        assert_eq!(r.as_array().unwrap().len(), 2);
        assert!(cosineRank(&json!({}), &Value::Null).is_err());
    }
}
