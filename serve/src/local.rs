//! THE LOCAL SEAT (RFC 0001 §8.1, phase E): what Heimdall needs to know about a model that runs on this machine before it may hand it work.
//!
//! A local worker is not a seat like the other six: there is no CLI to find and no sign-in to check, only a model server (Ollama) that may be down, may not hold
//! the model, and has one GPU. So everything here is a question Heimdall asks first, and every answer is "no, and why" until the operator has switched the seat on:
//!
//!   * `enabled()`        E7. OFF unless `ANTILOKI_LOCAL_WORKER=1`. With it off nothing in the product changes and no local seat is runnable.
//!   * `probe()` / `usable()`   E2. Reachable, the model installed, and a context set. A merge naming `local:<tag>` is runnable only then.
//!   * `Config::num_ctx`  E3. NEVER a default: Ollama's own default below 24 GB of VRAM is 4 096 tokens and silently cuts the start of a prompt, which looks like a dim model.
//!                        `ANTILOKI_LOCAL_NUM_CTX` must say how many. Without it the seat is unusable and says so.
//!   * `slot()`           E4. One fill at a time. A second request waits (and `queued()` says how many wait) instead of loading a second model beside the first: a wave of
//!                        eight on one GPU only queues (DIRECTION #3), and a model that does not fit beside the desktop pushes the machine into swap.
//!   * `Usage`, `watt_hours`, `electricity_usd`   E5. What a local call costs is electricity and time, never dollars of a subscription; the row says `seat = local` (in this codebase "technology" is the workspace's language, so the seat has its own word).
//!   * `escalate()`       E6. A fill that keeps failing its deterministic check goes to a named cloud worker once the retries are spent, and the result says it did.
//!
//! Nothing here calls a model on its own. `agent_local.rs` is the loop that does, through `slot()`.
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

/// The environment variable that switches the local seat on.
pub const FLAG: &str = "ANTILOKI_LOCAL_WORKER";

/// The key a merge uses for a local worker: `local:<ollama tag>`.
pub const KEY_PREFIX: &str = "local:";

/// E7: off unless asked for. `1`, `true` or `on`.
pub fn enabled() -> bool { std::env::var(FLAG).map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "on")).unwrap_or(false) }

pub fn is_local_key(key: &str) -> bool { key.starts_with(KEY_PREFIX) && key.len() > KEY_PREFIX.len() }
pub fn tag_of(key: &str) -> Option<&str> { key.strip_prefix(KEY_PREFIX).filter(|t| !t.trim().is_empty()) }

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The model server, `http://127.0.0.1:11434` unless `ANTILOKI_LOCAL_URL` says otherwise.
    pub base: String,
    /// E3: how many tokens of context every call asks for. None means "not set", which makes the seat unusable.
    pub num_ctx: Option<u32>,
    /// Memory (MB) that must stay free on the host while a model is resident. `ANTILOKI_LOCAL_HEADROOM_MB`, default 1500.
    pub headroom_mb: u64,
    /// The GPU's draw in watts while generating, for the electricity estimate. `ANTILOKI_LOCAL_GPU_WATTS`, default 170 (an RTX 3060's board power).
    pub gpu_watts: f64,
    /// Price of a kilowatt-hour in dollars. `ANTILOKI_LOCAL_KWH_USD`, default 0.15.
    pub kwh_usd: f64,
}

impl Config {
    pub fn from_env() -> Config {
        let get = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        Config {
            base: get("ANTILOKI_LOCAL_URL").unwrap_or_else(|| "http://127.0.0.1:11434".into()).trim_end_matches('/').to_string(),
            num_ctx: get("ANTILOKI_LOCAL_NUM_CTX").and_then(|v| v.parse().ok()).filter(|n| *n >= 1024),
            headroom_mb: get("ANTILOKI_LOCAL_HEADROOM_MB").and_then(|v| v.parse().ok()).unwrap_or(1500),
            gpu_watts: get("ANTILOKI_LOCAL_GPU_WATTS").and_then(|v| v.parse().ok()).unwrap_or(170.0),
            kwh_usd: get("ANTILOKI_LOCAL_KWH_USD").and_then(|v| v.parse().ok()).unwrap_or(0.15),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model { pub name: String, pub digest: String, pub size_bytes: u64, pub params: String }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe { pub reachable: bool, pub version: String, pub models: Vec<Model>, pub resident: Vec<String>, pub error: String }

fn client(secs: u64) -> reqwest::Client { reqwest::Client::builder().timeout(Duration::from_secs(secs)).connect_timeout(Duration::from_secs(2)).build().unwrap_or_default() }

/// Ask the model server what it has and what is loaded. Anything that does not answer is "not reachable", with the reason.
pub async fn probe(cfg: &Config) -> Probe {
    let c = client(4);
    let get = |p: &str| { let c = c.clone(); let url = format!("{}{p}", cfg.base); async move { c.get(&url).send().await.map_err(|e| e.to_string())?.json::<Value>().await.map_err(|e| e.to_string()) } };
    let tags = match get("/api/tags").await { Ok(v) => v, Err(e) => return Probe { error: format!("the model server at {} did not answer ({e})", cfg.base), ..Default::default() } };
    let models = tags["models"].as_array().into_iter().flatten().map(|m| Model {
        name: m["name"].as_str().unwrap_or("").to_string(), digest: m["digest"].as_str().unwrap_or("").to_string(),
        size_bytes: m["size"].as_u64().unwrap_or(0), params: m["details"]["parameter_size"].as_str().unwrap_or("").to_string() }).filter(|m| !m.name.is_empty()).collect();
    let resident = get("/api/ps").await.ok().map(|v| v["models"].as_array().into_iter().flatten().filter_map(|m| m["name"].as_str().map(String::from)).collect()).unwrap_or_default();
    let version = get("/api/version").await.ok().and_then(|v| v["version"].as_str().map(String::from)).unwrap_or_default();
    Probe { reachable: true, version, models, resident, error: String::new() }
}

/// E2 + E3: may this model be handed work right now? `Err` says why not, in words an operator can act on.
pub fn usable(cfg: &Config, p: &Probe, tag: &str) -> Result<(), String> {
    if !enabled() { return Err(format!("the local seat is switched off ({FLAG}=1 switches it on)")); }
    if cfg.num_ctx.is_none() { return Err("no context size is set (ANTILOKI_LOCAL_NUM_CTX, at least 1024): the model server's own default is 4096 tokens below 24 GB of VRAM and silently cuts the start of a prompt".into()); }
    if !p.reachable { return Err(if p.error.is_empty() { "the model server is not reachable".into() } else { p.error.clone() }); }
    let Some(m) = p.models.iter().find(|m| m.name == tag) else { return Err(format!("{tag} is not installed on the model server (installed: {})", p.models.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", "))); };
    // a model that is not loaded must fit beside the rest of the machine: Ollama holds more in host memory than the file's size says (a 6.6 GB model took about 10 GB)
    if !p.resident.iter().any(|r| r == tag) {
        if let Some(avail) = memory_available_mb() {
            let need = (m.size_bytes as f64 * 1.6 / 1_048_576.0) as u64 + cfg.headroom_mb;
            if avail < need { return Err(format!("{tag} needs about {need} MB free on this machine and {avail} MB are: loading it would push the machine into swap")); }
        }
    }
    Ok(())
}

/// Host memory that is available now (MB), from `/proc/meminfo`. None where there is no such file.
pub fn memory_available_mb() -> Option<u64> {
    let t = std::fs::read_to_string("/proc/meminfo").ok()?;
    t.lines().find_map(|l| l.strip_prefix("MemAvailable:")).and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok()).map(|kb| kb / 1024)
}

// ---------- E4: one at a time ----------

fn gate() -> &'static tokio::sync::Semaphore { static G: OnceLock<tokio::sync::Semaphore> = OnceLock::new(); G.get_or_init(|| tokio::sync::Semaphore::new(1)) }
static QUEUED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Held while a local model is working. Dropping it lets the next fill in.
pub struct Slot { _permit: tokio::sync::SemaphorePermit<'static> }

/// Wait for the one slot. Returns None if `max_wait` passes first (the caller reports "busy", it does not load a second model).
pub async fn slot(max_wait: Duration) -> Option<Slot> {
    QUEUED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let got = tokio::time::timeout(max_wait, gate().acquire()).await;
    QUEUED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    got.ok().and_then(Result::ok).map(|p| Slot { _permit: p })
}
/// How many fills are waiting for the slot (not counting the one running).
pub fn queued() -> usize { QUEUED.load(std::sync::atomic::Ordering::SeqCst) }

// ---------- E5: what a local call costs ----------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage { pub prompt_tokens: u64, pub completion_tokens: u64, pub ms: u64, pub calls: u32 }
impl Usage {
    pub fn add(&mut self, o: Usage) { self.prompt_tokens += o.prompt_tokens; self.completion_tokens += o.completion_tokens; self.ms += o.ms; self.calls += o.calls; }
    pub fn tokens_per_second(&self) -> f64 { if self.ms == 0 { 0.0 } else { self.completion_tokens as f64 / (self.ms as f64 / 1000.0) } }
}

/// Watt-hours a GPU drawing `watts` uses in `ms` milliseconds.
pub fn watt_hours(watts: f64, ms: u64) -> f64 { watts * (ms as f64 / 3_600_000.0) }
/// Dollars of electricity for `wh` watt-hours at `kwh_usd` a kilowatt-hour.
pub fn electricity_usd(wh: f64, kwh_usd: f64) -> f64 { wh / 1000.0 * kwh_usd }

/// The row the ledger keeps for a local call: no subscription dollars, the electricity estimate, and enough to see what ran.
pub fn ledger_row(cfg: &Config, tag: &str, digest: &str, u: &Usage) -> Value {
    let wh = watt_hours(cfg.gpu_watts, u.ms);
    json!({"seat": "local", "model": tag, "digest": digest, "num_ctx": cfg.num_ctx, "cost_usd": 0.0, "electricity_usd": electricity_usd(wh, cfg.kwh_usd), "gpu_wh": wh,
        "tokens_in": u.prompt_tokens, "tokens_out": u.completion_tokens, "tok_s": u.tokens_per_second(), "ms": u.ms, "calls": u.calls})
}

// ---------- E6: pay only on failure ----------

#[derive(Clone, Debug, PartialEq)]
pub struct Escalated<T> { pub value: T, /** the attempt that produced it (1-based); 0 when the cloud worker did */ pub attempt: u32, pub escalated: bool, pub attempts_used: u32 }

/// Run `attempt(n)` for n = 1..=1+retries until it is accepted (`Ok`); if every attempt fails, run `escalate` once. The error of the last attempt is what `escalate`
/// is told, so the cloud worker is given the real reason and not just "try again". `escalate = None` means no cloud worker is named: the last error is returned.
pub async fn escalate<T, F, Fut, G, Gf>(retries: u32, mut attempt: F, escalate: Option<G>) -> Result<Escalated<T>, String>
where F: FnMut(u32, Option<String>) -> Fut, Fut: std::future::Future<Output = Result<T, String>>, G: FnOnce(String) -> Gf, Gf: std::future::Future<Output = Result<T, String>> {
    let mut last: Option<String> = None;
    for n in 1..=1 + retries {
        match attempt(n, last.clone()).await { Ok(value) => return Ok(Escalated { value, attempt: n, escalated: false, attempts_used: n }), Err(e) => last = Some(e) }
    }
    let reason = last.unwrap_or_default();
    match escalate {
        Some(go) => go(reason).await.map(|value| Escalated { value, attempt: 0, escalated: true, attempts_used: 1 + retries }),
        None => Err(reason),
    }
}

// ---------- what Heimdall reads ----------

static LAST: OnceLock<std::sync::Mutex<Option<(Config, Probe)>>> = OnceLock::new();
fn last() -> &'static std::sync::Mutex<Option<(Config, Probe)>> { LAST.get_or_init(|| std::sync::Mutex::new(None)) }

/// Probe now and remember the answer, so the synchronous checks (`merge_runnable`) can read it.
pub async fn refresh() -> (Config, Probe) {
    let cfg = Config::from_env();
    let p = probe(&cfg).await;
    *last().lock().unwrap() = Some((cfg.clone(), p.clone()));
    (cfg, p)
}

/// The synchronous answer for a `local:<tag>` key: from the last probe. Never probed (or the flag off) is "not usable".
pub fn usable_cached(key: &str) -> bool {
    let Some(tag) = tag_of(key) else { return false };
    match last().lock().unwrap().as_ref() { Some((cfg, p)) => usable(cfg, p, tag).is_ok(), None => false }
}

/// The status the UI and the journeys read: the switch, the context, what the server has, why a given model is or is not usable.
pub fn status_json(cfg: &Config, p: &Probe) -> Value {
    json!({"enabled": enabled(), "base": cfg.base, "numCtx": cfg.num_ctx, "reachable": p.reachable, "error": p.error, "version": p.version, "queued": queued(), "memoryAvailableMb": memory_available_mb(),
        "models": p.models.iter().map(|m| json!({"name": m.name, "digest": m.digest, "sizeBytes": m.size_bytes, "params": m.params, "resident": p.resident.contains(&m.name),
            "usable": usable(cfg, p, &m.name).is_ok(), "why": usable(cfg, p, &m.name).err()})).collect::<Vec<_>>()})
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The environment is process-wide: tests that read or set it take this lock first.
    /// The slot is process-wide: tests that run a fill, or count who waits for it, take this first so they do not see each other.
    pub(crate) fn slot_serial() -> &'static tokio::sync::Mutex<()> { static L: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new(); L.get_or_init(|| tokio::sync::Mutex::new(())) }
    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> { static L: std::sync::Mutex<()> = std::sync::Mutex::new(()); L.lock().unwrap_or_else(|p| p.into_inner()) }

    /// A scripted Ollama: `answer(path, body) -> reply`.
    pub(crate) fn server(answer: impl Fn(&str, &Value) -> Value + Send + Sync + 'static) -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let answer = std::sync::Arc::new(answer);
        std::thread::spawn(move || for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let answer = answer.clone();
            std::thread::spawn(move || {
                let mut buf = vec![]; let mut chunk = [0u8; 8192];
                let (head_end, len) = loop {
                    let n = c.read(&mut chunk).unwrap_or(0); if n == 0 { return; }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                        break (i + 4, head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0));
                    }
                };
                while buf.len() < head_end + len { let n = c.read(&mut chunk).unwrap_or(0); if n == 0 { break; } buf.extend_from_slice(&chunk[..n]); }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
                let t = answer(&path, &body).to_string();
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{t}", t.len());
            });
        });
        format!("http://127.0.0.1:{port}")
    }

    fn ollama(models: &[(&str, u64)], resident: &[&str]) -> String {
        let models: Vec<Value> = models.iter().map(|(n, s)| json!({"name": n, "digest": format!("d-{n}"), "size": s, "details": {"parameter_size": "7B"}})).collect();
        let resident: Vec<Value> = resident.iter().map(|n| json!({"name": n})).collect();
        server(move |path, _| match path { "/api/tags" => json!({"models": models}), "/api/ps" => json!({"models": resident}), "/api/version" => json!({"version": "0.test"}), _ => json!({}) })
    }

    fn cfg(base: &str, ctx: Option<u32>) -> Config { Config { base: base.into(), num_ctx: ctx, headroom_mb: 0, gpu_watts: 170.0, kwh_usd: 0.15 } }

    #[tokio::test]
    async fn the_seat_is_off_until_it_is_switched_on_and_says_so() {
        let _g = env_lock();
        std::env::remove_var(FLAG);
        let base = ollama(&[("qwen2.5-coder:14b", 9_000_000_000)], &[]);
        let c = cfg(&base, Some(65536));
        let p = probe(&c).await;
        assert!(p.reachable && p.models.len() == 1 && p.version == "0.test");
        assert!(usable(&c, &p, "qwen2.5-coder:14b").unwrap_err().contains("switched off"), "E7: off by default");
        std::env::set_var(FLAG, "1");
        assert!(enabled());
        // memory is the host's; give the check nothing to refuse on by naming a tiny model size in a second server
        let tiny = ollama(&[("tiny:1b", 1_000)], &[]);
        let c2 = cfg(&tiny, Some(65536));
        let p2 = probe(&c2).await;
        assert_eq!(usable(&c2, &p2, "tiny:1b"), Ok(()));
        std::env::remove_var(FLAG);
    }

    #[tokio::test]
    async fn a_context_size_is_required_never_defaulted() {
        let _g = env_lock();
        std::env::set_var(FLAG, "1");
        let base = ollama(&[("tiny:1b", 1_000)], &[]);
        let p = probe(&cfg(&base, None)).await;
        let e = usable(&cfg(&base, None), &p, "tiny:1b").unwrap_err();
        assert!(e.contains("ANTILOKI_LOCAL_NUM_CTX") && e.contains("4096"), "E3: {e}");
        assert!(usable(&cfg(&base, Some(65536)), &p, "tiny:1b").is_ok());
        std::env::remove_var(FLAG);
    }

    #[tokio::test]
    async fn a_missing_model_or_a_dead_server_is_not_usable_and_says_why() {
        let _g = env_lock();
        std::env::set_var(FLAG, "1");
        let base = ollama(&[("tiny:1b", 1_000)], &[]);
        let c = cfg(&base, Some(8192));
        let p = probe(&c).await;
        let e = usable(&c, &p, "other:7b").unwrap_err();
        assert!(e.contains("not installed") && e.contains("tiny:1b"), "{e}");
        let dead = cfg("http://127.0.0.1:1", Some(8192));
        let pd = probe(&dead).await;
        assert!(!pd.reachable && usable(&dead, &pd, "tiny:1b").unwrap_err().contains("did not answer"));
        std::env::remove_var(FLAG);
    }

    #[tokio::test]
    async fn a_model_that_cannot_fit_beside_the_machine_is_refused_before_it_loads() {
        let _g = env_lock();
        std::env::set_var(FLAG, "1");
        let huge = ollama(&[("huge:70b", 400_000_000_000), ("small:1b", 1_000)], &["small:1b"]);
        let c = cfg(&huge, Some(8192));
        let p = probe(&c).await;
        let e = usable(&c, &p, "huge:70b").unwrap_err();
        assert!(e.contains("swap"), "{e}");
        assert!(usable(&c, &p, "small:1b").is_ok(), "a model that is already loaded is not asked to fit again");
        std::env::remove_var(FLAG);
    }

    #[tokio::test]
    async fn the_cached_answer_is_what_merge_runnable_reads() {
        let _g = env_lock();
        std::env::set_var(FLAG, "1");
        std::env::set_var("ANTILOKI_LOCAL_NUM_CTX", "32768");
        let base = ollama(&[("tiny:1b", 1_000)], &[]);
        std::env::set_var("ANTILOKI_LOCAL_URL", &base);
        refresh().await;
        assert!(usable_cached("local:tiny:1b"));
        assert!(!usable_cached("local:nothing:1b"));
        assert!(!usable_cached("claude-code:sonnet"), "only local keys");
        assert_eq!(tag_of("local:qwen3:14b"), Some("qwen3:14b"));
        assert!(is_local_key("local:x") && !is_local_key("local:") && !is_local_key("opencode:x"));
        for k in [FLAG, "ANTILOKI_LOCAL_NUM_CTX", "ANTILOKI_LOCAL_URL"] { std::env::remove_var(k); }
    }

    #[tokio::test]
    async fn only_one_fill_holds_the_slot_and_the_rest_wait() {
        let _s = slot_serial().lock().await;
        let first = slot(Duration::from_secs(1)).await.expect("the slot is free");
        let t0 = std::time::Instant::now();
        let waiting = tokio::spawn(async { slot(Duration::from_secs(5)).await.is_some() });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(queued(), 1, "E4: a second fill waits and is counted");
        assert!(slot(Duration::from_millis(50)).await.is_none(), "and a third that will not wait is told the seat is busy");
        drop(first);
        assert!(waiting.await.unwrap(), "the waiting fill got in once the first finished");
        assert!(t0.elapsed() < Duration::from_secs(5));
        assert_eq!(queued(), 0);
    }

    #[test]
    fn a_local_call_costs_electricity_and_time_never_subscription_dollars() {
        let c = cfg("http://x", Some(65536));
        let u = Usage { prompt_tokens: 1000, completion_tokens: 500, ms: 20_000, calls: 2 };
        let row = ledger_row(&c, "qwen2.5-coder:14b", "d1", &u);
        assert_eq!(row["seat"], "local");
        assert_eq!(row["cost_usd"], 0.0);
        let wh = row["gpu_wh"].as_f64().unwrap();
        assert!((wh - 170.0 * 20.0 / 3600.0).abs() < 1e-9, "{wh}");
        assert!((row["electricity_usd"].as_f64().unwrap() - wh / 1000.0 * 0.15).abs() < 1e-12);
        assert_eq!(row["tok_s"], 25.0);
        assert_eq!(row["num_ctx"], 65536);
        let mut t = Usage::default(); t.add(u); t.add(u);
        assert_eq!((t.completion_tokens, t.ms, t.calls), (1000, 40_000, 4));
    }

    #[tokio::test]
    async fn a_fill_that_keeps_failing_goes_to_the_cloud_worker_with_the_real_reason() {
        let r = escalate(2, |n, last| async move { if n == 2 { assert_eq!(last.as_deref(), Some("test failed on try 1")); } Err::<String, _>(format!("test failed on try {n}")) },
            Some(|reason: String| async move { Ok::<_, String>(format!("cloud fixed it ({reason})")) })).await.unwrap();
        assert!(r.escalated && r.attempt == 0 && r.attempts_used == 3);
        assert_eq!(r.value, "cloud fixed it (test failed on try 3)", "the cloud worker is told why the last local attempt failed");
    }

    #[tokio::test]
    async fn a_fill_that_passes_on_a_retry_never_reaches_the_cloud() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let r = escalate(2, |n, _| { calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst); async move { if n == 2 { Ok("fine".to_string()) } else { Err("not yet".to_string()) } } },
            Some(|_: String| async { Err::<String, String>("the cloud must not be called".into()) })).await.unwrap();
        assert!(!r.escalated && r.attempt == 2 && r.value == "fine");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let none: Result<Escalated<String>, String> = escalate(1, |n, _| async move { Err(format!("no {n}")) }, None::<fn(String) -> std::future::Ready<Result<String, String>>>).await;
        assert_eq!(none.unwrap_err(), "no 2", "with no cloud worker named the last error stands");
    }

    #[test]
    fn the_context_must_be_a_real_number() {
        let _g = env_lock();
        std::env::set_var("ANTILOKI_LOCAL_NUM_CTX", "12");
        assert_eq!(Config::from_env().num_ctx, None, "under 1024 tokens is not a context an agent can work in");
        std::env::set_var("ANTILOKI_LOCAL_NUM_CTX", "65536");
        assert_eq!(Config::from_env().num_ctx, Some(65536));
        std::env::remove_var("ANTILOKI_LOCAL_NUM_CTX");
        assert_eq!(Config::from_env().num_ctx, None, "no default");
    }
}

// ---------- the routes ----------

use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};

/// `GET /api/v1/local/status`: the switch, the context, what the model server has, and for each model whether it is usable and why not.
pub async fn http_status() -> Response {
    let (cfg, p) = refresh().await;
    Json(status_json(&cfg, &p)).into_response()
}

/// `POST /api/v1/local/fill` `{model, prompt, cwd, scope: [globs], max_turns?}`: one fill by the local worker, through the one slot, behind the flag and the usability
/// check. Returns what was written, what the wall refused, the tokens, and the ledger row (electricity, never subscription dollars).
pub async fn http_fill(Json(b): Json<Value>) -> Response {
    let fail = |code: StatusCode, why: String| (code, Json(json!({"ok": false, "error": why}))).into_response();
    let tag = b["model"].as_str().unwrap_or("").trim().to_string();
    let cwd = std::path::PathBuf::from(b["cwd"].as_str().unwrap_or(""));
    if tag.is_empty() || b["prompt"].as_str().unwrap_or("").trim().is_empty() { return fail(StatusCode::BAD_REQUEST, "model and prompt are required".into()); }
    if !cwd.is_absolute() || !cwd.is_dir() { return fail(StatusCode::BAD_REQUEST, "cwd must be an existing absolute folder".into()); }
    let (cfg, p) = refresh().await;
    if let Err(why) = usable(&cfg, &p, &tag) { return fail(StatusCode::CONFLICT, why); }
    let scope: Vec<String> = b["scope"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(String::from)).collect();
    let mut fill = crate::agent_local::Fill::new(b["prompt"].as_str().unwrap_or(""), &cwd, &[]);
    fill.scope = scope;
    if let Some(n) = b["max_turns"].as_u64() { fill.max_turns = n.clamp(1, 40) as u32; }
    match crate::agent_local::run_fill(&cfg, &tag, &fill).await {
        Ok(o) => {
            let digest = p.models.iter().find(|m| m.name == tag).map(|m| m.digest.clone()).unwrap_or_default();
            Json(json!({"ok": true, "finished": o.finished, "summary": o.summary, "changed": o.changed, "refused": o.refused, "turns": o.turns, "ledger": ledger_row(&cfg, &tag, &digest, &o.usage)})).into_response()
        }
        Err(e) => fail(StatusCode::BAD_GATEWAY, e),
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use super::tests::{env_lock, server};

    async fn body(r: Response) -> (StatusCode, Value) {
        let (parts, b) = r.into_parts();
        let bytes = axum::body::to_bytes(b, 1 << 20).await.unwrap();
        (parts.status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn the_status_route_says_what_is_usable_and_why_not() {
        let _g = env_lock();
        let base = server(|path, _| match path { "/api/tags" => json!({"models": [{"name": "tiny:1b", "digest": "d1", "size": 1000, "details": {"parameter_size": "1B"}}]}), _ => json!({}) });
        std::env::set_var("ANTILOKI_LOCAL_URL", &base);
        std::env::remove_var(FLAG); std::env::remove_var("ANTILOKI_LOCAL_NUM_CTX");
        let (code, v) = body(http_status().await).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(v["enabled"], false);
        assert_eq!(v["models"][0]["usable"], false);
        assert!(v["models"][0]["why"].as_str().unwrap().contains("switched off"));
        std::env::set_var(FLAG, "1"); std::env::set_var("ANTILOKI_LOCAL_NUM_CTX", "16384");
        let (_, v) = body(http_status().await).await;
        assert_eq!((v["enabled"].clone(), v["numCtx"].clone(), v["models"][0]["usable"].clone()), (json!(true), json!(16384), json!(true)));
        for k in [FLAG, "ANTILOKI_LOCAL_NUM_CTX", "ANTILOKI_LOCAL_URL"] { std::env::remove_var(k); }
    }

    #[tokio::test]
    async fn a_fill_through_the_route_is_refused_until_the_seat_is_on_and_then_writes_inside_the_scope() {
        let _s = super::tests::slot_serial().lock().await;
        let _g = env_lock();
        let dir = std::env::temp_dir().join(format!("antiloki-local-route-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let step = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let s2 = step.clone();
        let base = server(move |path, _| match path {
            "/api/tags" => json!({"models": [{"name": "tiny:1b", "digest": "d1", "size": 1000, "details": {}}]}),
            "/api/chat" => { let mut n = s2.lock().unwrap(); *n += 1;
                if *n == 1 { json!({"message": {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "write_file", "arguments": {"path": "out/a.txt", "content": "from the local worker"}}}]}, "prompt_eval_count": 50, "eval_count": 10}) }
                else { json!({"message": {"role": "assistant", "content": "", "tool_calls": [{"function": {"name": "finish", "arguments": {"summary": "wrote it"}}}]}, "prompt_eval_count": 60, "eval_count": 5}) } }
            _ => json!({}) });
        std::env::set_var("ANTILOKI_LOCAL_URL", &base);
        std::env::remove_var(FLAG); std::env::remove_var("ANTILOKI_LOCAL_NUM_CTX");
        let req = || Json(json!({"model": "tiny:1b", "prompt": "create out/a.txt", "cwd": dir, "scope": ["out/**"]}));
        let (code, v) = body(http_fill(req()).await).await;
        assert_eq!(code, StatusCode::CONFLICT, "off by default: {v}");
        assert!(!dir.join("out/a.txt").exists());
        std::env::set_var(FLAG, "1"); std::env::set_var("ANTILOKI_LOCAL_NUM_CTX", "16384");
        let (code, v) = body(http_fill(req()).await).await;
        assert_eq!(code, StatusCode::OK, "{v}");
        assert_eq!((v["finished"].clone(), v["changed"].clone()), (json!(true), json!(["out/a.txt"])));
        assert_eq!(std::fs::read_to_string(dir.join("out/a.txt")).unwrap(), "from the local worker");
        assert_eq!(v["ledger"]["seat"], "local");
        assert_eq!(v["ledger"]["cost_usd"], 0.0);
        assert_eq!(v["ledger"]["num_ctx"], 16384);
        assert_eq!(v["ledger"]["tokens_in"], 110);
        let (code, _) = body(http_fill(Json(json!({"model": "tiny:1b", "prompt": "x", "cwd": "relative/path"}))).await).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "the box must be an absolute folder");
        for k in [FLAG, "ANTILOKI_LOCAL_NUM_CTX", "ANTILOKI_LOCAL_URL"] { std::env::remove_var(k); }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
