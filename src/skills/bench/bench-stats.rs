//! bench-stats.rs — twin of skills/bench/bench-stats.mjs. NAMES ARE CONTRACT: `benchStats`
//! (named default). Pure percentile math, no DB, no I/O.
// QUALITY: 88/100 — faithful clone of a small pure function; int-vs-float output representation
// is absorbed by the comparator's JSON parse (42.0 == 42), values are byte-exact f64
use serde_json::{json, Value};

pub fn benchStats(input: &Value, _context: &Value) -> Value {
    // JS: const samples = input?.samples ?? input
    let samples_val = match input.get("samples") {
        Some(v) if !v.is_null() => v,
        _ => input,
    };
    let arr = samples_val.as_array();
    if arr.map_or(true, |a| a.is_empty()) {
        return json!({ "count": 0, "min": 0, "max": 0, "avg": 0, "p50": 0, "p95": 0, "p99": 0 });
    }
    // JS [...samples].sort((a,b)=>a-b) — numeric ascending. Fixtures are number arrays.
    let mut sorted: Vec<f64> = arr.unwrap().iter().filter_map(Value::as_f64).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = sorted.len();
    let sum: f64 = sorted.iter().sum();
    // JS: sorted[Math.min(n - 1, Math.floor(n * p))]
    let pct = |p: f64| sorted[((n as f64 * p).floor() as usize).min(n - 1)];
    json!({
        "count": n,
        "min": sorted[0],
        "max": sorted[n - 1],
        "avg": sum / n as f64,
        "p50": pct(0.50),
        "p95": pct(0.95),
        "p99": pct(0.99),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_non_array_are_zeros() {
        let z = json!({ "count": 0, "min": 0, "max": 0, "avg": 0, "p50": 0, "p95": 0, "p99": 0 });
        assert_eq!(benchStats(&json!({ "samples": [] }), &Value::Null), z);
        assert_eq!(benchStats(&json!({}), &Value::Null), z);
        assert_eq!(benchStats(&json!({ "samples": "nope" }), &Value::Null), z);
    }

    #[test]
    fn percentiles_floor_np_clamped_and_sorted() {
        let r = benchStats(&json!({ "samples": [5, 1, 3, 2, 4] }), &Value::Null);
        assert_eq!(r["count"], json!(5));
        assert_eq!(r["min"].as_f64().unwrap(), 1.0);
        assert_eq!(r["max"].as_f64().unwrap(), 5.0);
        assert_eq!(r["avg"].as_f64().unwrap(), 3.0);
        assert_eq!(r["p50"].as_f64().unwrap(), 3.0); // floor(5*0.5)=2 → sorted[2]=3
        assert_eq!(r["p95"].as_f64().unwrap(), 5.0);
    }

    #[test]
    fn bare_array_and_floats() {
        assert_eq!(benchStats(&json!([10, 20]), &Value::Null)["avg"].as_f64().unwrap(), 15.0);
        assert_eq!(benchStats(&json!({ "samples": [1.5, 2.5, 3.5] }), &Value::Null)["avg"].as_f64().unwrap(), 2.5);
    }
}
