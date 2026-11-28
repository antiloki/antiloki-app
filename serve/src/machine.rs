//! THE MACHINE WATCHER (operator 2026-11-21: the bottom bar's bell "is useless as a notification, but useful as a RAM, VRAM and CPU watcher for avoiding freezing the agents or the machine when using local models"):
//! CPU, RAM and VRAM now, and who is using them. `GET /api/v1/machine`. Linux answers all of it (`/proc`, `nvidia-smi`, the amdgpu sysfs files); where a number cannot be read it is `null`, never a guess. Pure parsers, tested; `snapshot` reads the machine.
use serde_json::{json, Value};
use std::sync::Mutex;

/// The CPU counters of `/proc/stat`'s first line: (busy, total) jiffies. Pure.
pub fn parse_cpu(text: &str) -> Option<(u64, u64)> {
    let l = text.lines().next()?; let f: Vec<u64> = l.strip_prefix("cpu")?.split_whitespace().filter_map(|x| x.parse().ok()).collect(); if f.len() < 4 { return None }
    let idle = f[3] + f.get(4).copied().unwrap_or(0); let total: u64 = f.iter().take(8).sum(); Some((total.saturating_sub(idle), total))
}
/// Percent busy between two readings. Pure.
pub fn cpu_pct(prev: (u64, u64), now: (u64, u64)) -> f64 { let dt = now.1.saturating_sub(prev.1); if dt == 0 { 0.0 } else { (now.0.saturating_sub(prev.0) as f64 / dt as f64 * 100.0).clamp(0.0, 100.0) } }
/// `nvidia-smi --query-gpu=memory.used,memory.total --format=csv,noheader,nounits` → (used MB, total MB) summed over GPUs. Pure.
pub fn parse_nvidia(text: &str) -> Option<(u64, u64)> {
    let (mut u, mut t, mut n) = (0u64, 0u64, 0); for l in text.lines() { let mut p = l.split(','); if let (Some(a), Some(b)) = (p.next().and_then(|x| x.trim().parse::<u64>().ok()), p.next().and_then(|x| x.trim().parse::<u64>().ok())) { u += a; t += b; n += 1; } }
    if n == 0 || t == 0 { None } else { Some((u, t)) }
}
/// How a share of memory reads: calm below 80%, loaded to 92%, danger above (the point where a desktop starts to swap and an agent stalls). Pure.
pub fn tone(pct: f64) -> &'static str { if pct >= 92.0 { "danger" } else if pct >= 80.0 { "loaded" } else { "calm" } }
/// What a process is, by its command line, for the list. Pure.
pub fn kind_of(cmd: &str) -> &'static str {
    let c = cmd.to_ascii_lowercase();
    if c.contains("ollama") || c.contains("llama") || c.contains("vllm") || c.contains("lmstudio") { "local model" } else if c.contains("claude") || c.contains("codex") || c.contains("opencode") || c.contains("gemini") || c.contains("cursor-agent") || c.contains("antiloki-agent") { "agent" }
    else if c.contains("antiloki-serve") { "service" } else if c.contains("vite") || c.contains("node") || c.contains("cargo") || c.contains("rustc") { "dev" } else { "other" }
}
/// `ps` lines `pid pcpu rss args` → the heaviest by memory. Pure.
pub fn parse_ps(text: &str, take: usize) -> Vec<Value> {
    let mut v: Vec<(u64, Value)> = text.lines().filter_map(|l| { let mut p = l.trim().splitn(4, char::is_whitespace); let pid = p.next()?.parse::<u64>().ok()?; let cpu = p.next()?.trim().parse::<f64>().ok()?; let rss = p.next()?.trim().parse::<u64>().ok()?; let args = p.next()?.trim().to_string();
        let name: String = args.split_whitespace().next().unwrap_or("").rsplit('/').next().unwrap_or("").chars().take(40).collect(); Some((rss, json!({ "pid": pid, "name": name, "kind": kind_of(&args), "cpu": cpu, "ram_kb": rss }))) }).collect();
    v.sort_by(|a, b| b.0.cmp(&a.0)); v.into_iter().take(take).map(|x| x.1).collect()
}

/// Windows' own CPU counters (`GetSystemTimes`: idle, kernel INCLUDING idle, user) → (busy, total). Pure.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn cpu_from_times(idle: u64, kernel: u64, user: u64) -> (u64, u64) { let total = kernel.saturating_add(user); (total.saturating_sub(idle), total) }
/// `tasklist /fo csv /nh` lines (name, pid, session, session number, memory as "12,345 K" in the machine's own number format) → the heaviest by memory. It says no CPU share, so that is null. Pure.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn parse_tasklist(text: &str, take: usize) -> Vec<Value> {
    let mut v: Vec<(u64, Value)> = text.lines().filter_map(|l| { let f: Vec<&str> = l.trim().strip_prefix('"')?.strip_suffix('"')?.split("\",\"").collect(); if f.len() < 5 { return None }
        let pid = f[1].parse::<u64>().ok()?; let kb: u64 = f[4].chars().filter(char::is_ascii_digit).collect::<String>().parse().ok()?; let name: String = f[0].chars().take(40).collect(); Some((kb, json!({ "pid": pid, "name": name, "kind": kind_of(f[0]), "cpu": Value::Null, "ram_kb": kb }))) }).collect();
    v.sort_by(|a, b| b.0.cmp(&a.0)); v.into_iter().take(take).map(|x| x.1).collect()
}

#[cfg(not(windows))]
fn cpu_counters() -> Option<(u64, u64)> { std::fs::read_to_string("/proc/stat").ok().and_then(|t| parse_cpu(&t)) }
#[cfg(windows)]
fn cpu_counters() -> Option<(u64, u64)> {
    #[repr(C)] #[derive(Default)] struct FileTime { low: u32, high: u32 }
    #[link(name = "kernel32")] extern "system" { fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32; }
    let (mut i, mut k, mut u) = (FileTime::default(), FileTime::default(), FileTime::default());
    if unsafe { GetSystemTimes(&mut i, &mut k, &mut u) } == 0 { return None }
    let n = |t: &FileTime| (u64::from(t.high) << 32) | u64::from(t.low); Some(cpu_from_times(n(&i), n(&k), n(&u)))
}
#[cfg(not(windows))]
fn processes() -> Vec<Value> { crate::proc::cmd("ps").args(["-eo", "pid=,pcpu=,rss=,args=", "--sort=-rss"]).output().ok().filter(|o| o.status.success()).map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout), 8)).unwrap_or_default() }
#[cfg(windows)]
fn processes() -> Vec<Value> { crate::proc::cmd("tasklist").args(["/fo", "csv", "/nh"]).output().ok().filter(|o| o.status.success()).map(|o| parse_tasklist(&String::from_utf8_lossy(&o.stdout), 8)).unwrap_or_default() }

static LAST_CPU: Mutex<Option<(u64, u64)>> = Mutex::new(None);
/// The total VRAM of the GPU(s) in MB, or None when there is no GPU to read (no NVIDIA driver, no amdgpu): local models then run on the CPU.
pub fn vram_total_mb() -> Option<u64> { vram().map(|(_, t)| t).filter(|t| *t > 0) }
fn vram() -> Option<(u64, u64)> {
    if let Ok(o) = crate::proc::cmd("nvidia-smi").args(["--query-gpu=memory.used,memory.total", "--format=csv,noheader,nounits"]).output() { if o.status.success() { if let Some(x) = parse_nvidia(&String::from_utf8_lossy(&o.stdout)) { return Some(x) } } }
    // AMD / Intel Arc through the kernel's own files
    let (mut u, mut t) = (0u64, 0u64); for e in std::fs::read_dir("/sys/class/drm").into_iter().flatten().flatten() { let d = e.path().join("device"); let (Some(a), Some(b)) = (std::fs::read_to_string(d.join("mem_info_vram_used")).ok().and_then(|x| x.trim().parse::<u64>().ok()), std::fs::read_to_string(d.join("mem_info_vram_total")).ok().and_then(|x| x.trim().parse::<u64>().ok())) else { continue }; u += a / 1_048_576; t += b / 1_048_576; }
    if t > 0 { Some((u, t)) } else { None }
}
/// The machine now. Reads `/proc`, one `ps`, and the GPU; `null` for what this machine cannot say.
pub fn snapshot() -> Value {
    let cpu = cpu_counters().and_then(|now| { let mut g = LAST_CPU.lock().unwrap_or_else(|p| p.into_inner()); let r = g.map(|p| cpu_pct(p, now)); *g = Some(now); r });
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    let ram = crate::mem::system().map(|(total, avail)| { let used = total.saturating_sub(avail); json!({ "used_mb": used / 1_048_576, "total_mb": total / 1_048_576, "pct": used as f64 / total.max(1) as f64 * 100.0 }) });
    let vr = vram().map(|(u, t)| json!({ "used_mb": u, "total_mb": t, "pct": u as f64 / t as f64 * 100.0 }));
    let procs = processes();
    let worst = [ram.as_ref().and_then(|r| r["pct"].as_f64()), vr.as_ref().and_then(|r| r["pct"].as_f64())].into_iter().flatten().fold(0.0, f64::max);
    json!({ "ok": true, "t_ms": crate::cli::now_ms(), "cpu": { "pct": cpu, "cores": cores }, "ram": ram, "vram": vr, "tone": tone(worst), "procs": procs })
}
/// The models a local runner (Ollama) has loaded, from `ollama ps` (NAME is the first column). Pure.
pub fn parse_ollama_ps(text: &str) -> Vec<String> { text.lines().skip(1).filter_map(|l| l.split_whitespace().next()).filter(|n| !n.is_empty()).map(String::from).collect() }
/// Unload every model Ollama holds in memory (`ollama stop <name>`): the model files stay, the RAM and VRAM come back. Says what it did; nothing is killed. Blocking. The person's click is the gate (the screen asks first).
pub fn unload_local() -> Value {
    let Ok(o) = crate::proc::cmd("ollama").arg("ps").output() else { return json!({ "ok": false, "error": "ollama is not installed here: nothing to unload" }) };
    if !o.status.success() { return json!({ "ok": false, "error": "ollama did not answer (is it running?)" }) }
    let names = parse_ollama_ps(&String::from_utf8_lossy(&o.stdout)); if names.is_empty() { return json!({ "ok": true, "unloaded": [], "note": "no model was loaded" }) }
    let mut done = vec![]; let mut failed = vec![]; for n in names { match crate::proc::cmd("ollama").args(["stop", &n]).output() { Ok(r) if r.status.success() => done.push(n), _ => failed.push(n) } }
    json!({ "ok": failed.is_empty(), "unloaded": done, "failed": failed })
}
pub async fn unload_http(body: axum::body::Bytes) -> axum::response::Response { use axum::response::IntoResponse; let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let v = if b["pane_id"].as_i64().unwrap_or(0) > 0 { json!({ "ok": false, "error": "only the person unloads a model" }) } else { tokio::task::spawn_blocking(unload_local).await.unwrap_or_else(|_| json!({ "ok": false, "error": "cut off" })) }; (axum::http::StatusCode::OK, axum::Json(v)).into_response() }
pub async fn http() -> axum::response::Response { use axum::response::IntoResponse; let v = tokio::task::spawn_blocking(snapshot).await.unwrap_or_else(|_| json!({ "ok": false, "error": "cut off" })); (axum::http::StatusCode::OK, axum::Json(v)).into_response() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn cpu_is_the_busy_share_between_two_readings() {
        let a = parse_cpu("cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 1 2 3 4").unwrap(); let b = parse_cpu("cpu  200 0 200 1000 0 0 0 0 0 0").unwrap();
        assert_eq!(a, (200, 1000)); assert!((cpu_pct(a, b) - 50.0).abs() < 1e-9); assert_eq!(cpu_pct(a, a), 0.0); assert!(parse_cpu("nothing").is_none());
    }
    #[test] fn vram_sums_the_gpus_and_says_nothing_when_there_is_none() {
        assert_eq!(parse_nvidia("10240, 12288\n 512, 8192\n"), Some((10752, 20480))); assert_eq!(parse_nvidia("NVIDIA-SMI has failed"), None); assert_eq!(parse_nvidia(""), None);
    }
    #[test] fn the_loaded_models_are_the_first_column_of_ollama_ps() { assert_eq!(parse_ollama_ps("NAME ID SIZE PROCESSOR UNTIL\nqwen3-coder:30b abc 19 GB 100% GPU 4 minutes\nllama3:8b d 5 GB 100% GPU 2 minutes\n"), vec!["qwen3-coder:30b", "llama3:8b"]); assert!(parse_ollama_ps("NAME ID\n").is_empty()); assert!(parse_ollama_ps("").is_empty()); }
    #[test] fn the_tone_turns_at_80_and_92() { assert_eq!(tone(79.9), "calm"); assert_eq!(tone(80.0), "loaded"); assert_eq!(tone(91.9), "loaded"); assert_eq!(tone(92.0), "danger"); }
    #[test] fn the_heaviest_processes_are_listed_and_named_by_what_they_are() {
        let ps = " 11 3.0 900000 /usr/bin/ollama runner --model qwen\n 12 1.0 1400000 claude --resume\n 13 0.5 2000 bash\n 14 9.0 700000 node /x/vite.js\n";
        let v = parse_ps(ps, 3); assert_eq!(v.len(), 3); assert_eq!(v[0]["name"], json!("claude")); assert_eq!(v[0]["kind"], json!("agent")); assert_eq!(v[1]["kind"], json!("local model")); assert_eq!(v[2]["kind"], json!("dev"));
        assert_eq!(kind_of("antiloki-serve --port 1"), "service");
    }
    #[test] fn windows_counters_and_its_process_list_are_read() {
        assert_eq!(cpu_from_times(600, 800, 200), (400, 1000));
        let t = "\"claude.exe\",\"12\",\"Console\",\"1\",\"1.400.000 K\"\r\n\"node.exe\",\"13\",\"Console\",\"1\",\"700,000 K\"\r\nINFO: nothing\r\n";
        let v = parse_tasklist(t, 8); assert_eq!(v.len(), 2); assert_eq!((v[0]["name"].clone(), v[0]["ram_kb"].clone(), v[0]["kind"].clone(), v[0]["cpu"].clone()), (json!("claude.exe"), json!(1_400_000), json!("agent"), Value::Null));
    }
    #[test] fn this_machine_answers_with_numbers_or_nulls_never_a_crash() { let s = snapshot(); assert_eq!(s["ok"], json!(true)); assert!(s["tone"].is_string()); if cfg!(target_os = "linux") { assert!(s["ram"]["total_mb"].as_u64().unwrap_or(0) > 0); } }
}
