//! STALE WHILE REVALIDATE: the last answer is served at once, and when it is older than `fresh` a single recompute runs in the background. Used for answers that are costly to make and change slowly (the machine's engines, the saga list, the forge tokens): the page asks for them on every load and every few seconds, and making them each time (a probe that shells out, a record of hundreds of runs) kept the serve busy and the page waiting. Only the very first ask waits. An error is never kept: the caller sees it on a cold ask, and a failed background refresh leaves the old answer in place. Blocking: call it inside `spawn_blocking`.
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Store { vals: HashMap<String, (Instant, Value)>, busy: HashSet<String> }
fn store() -> &'static Mutex<Store> { static S: OnceLock<Mutex<Store>> = OnceLock::new(); S.get_or_init(Default::default) }

/// The answer for `key`: kept for `fresh`, then served stale while one background recompute replaces it.
pub fn get(key: &str, fresh: Duration, compute: impl Fn() -> Result<Value, String> + Send + 'static) -> Result<Value, String> {
    let hit = store().lock().unwrap_or_else(|e| e.into_inner()).vals.get(key).cloned();
    match hit {
        Some((at, v)) => {
            if at.elapsed() >= fresh && store().lock().unwrap_or_else(|e| e.into_inner()).busy.insert(key.to_string()) {
                let k = key.to_string();
                std::thread::spawn(move || {
                    let r = compute();
                    let mut s = store().lock().unwrap_or_else(|e| e.into_inner()); if let Ok(v) = r { s.vals.insert(k.clone(), (Instant::now(), v)); } s.busy.remove(&k);
                });
            }
            Ok(v)
        }
        None => { let v = compute()?; store().lock().unwrap_or_else(|e| e.into_inner()).vals.insert(key.to_string(), (Instant::now(), v.clone())); Ok(v) }
    }
}
/// The kept answer for `key`, if one was ever made.
pub fn peek(key: &str) -> Option<Value> { store().lock().unwrap_or_else(|e| e.into_inner()).vals.get(key).map(|(_, v)| v.clone()) }
/// Forget every answer whose key starts with `prefix` (after something changed that they show).
pub fn forget(prefix: &str) { store().lock().unwrap_or_else(|e| e.into_inner()).vals.retain(|k, _| !k.starts_with(prefix)); }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn an_answer_is_made_once_served_stale_and_replaced_in_the_background() {
        let n = Arc::new(AtomicUsize::new(0)); let mk = |n: Arc<AtomicUsize>| move || { let c = n.fetch_add(1, Ordering::SeqCst) + 1; Ok(json!({ "n": c })) };
        assert_eq!(get("t:a", Duration::from_secs(60), mk(n.clone())).unwrap()["n"], json!(1), "the first ask makes it");
        assert_eq!(get("t:a", Duration::from_secs(60), mk(n.clone())).unwrap()["n"], json!(1), "a fresh answer is not made again");
        assert_eq!(get("t:a", Duration::from_millis(0), mk(n.clone())).unwrap()["n"], json!(1), "a stale answer is served at once");
        for _ in 0..50 { if n.load(Ordering::SeqCst) >= 2 { break; } std::thread::sleep(Duration::from_millis(50)); } std::thread::sleep(Duration::from_millis(100));
        assert_eq!(get("t:a", Duration::from_secs(60), mk(n.clone())).unwrap()["n"], json!(2), "and the background recompute replaced it");
        forget("t:"); assert_eq!(get("t:a", Duration::from_secs(60), mk(n.clone())).unwrap()["n"], json!(3), "forgotten means made again");
    }
    #[test]
    fn an_error_is_not_kept() {
        assert!(get("t:err", Duration::from_secs(60), || Err("no".into())).is_err());
        assert_eq!(get("t:err", Duration::from_secs(60), || Ok(json!(1))).unwrap(), json!(1), "the next ask tries again");
        assert_eq!(get("t:err", Duration::from_millis(0), || Err("later".into())).unwrap(), json!(1), "a failed refresh keeps the old answer");
    }
}
