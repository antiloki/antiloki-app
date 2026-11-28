//! WHAT THE AI CLIs REALLY USED (operator 2026-11-21: the usage window must "compare exactly the AIs used with their price on OpenRouter"): the agents run inside Claude Code, Codex and the other CLIs, which write every API call with its model and token counts into their own session files
//! (`~/.claude/projects/**/*.jsonl`, `~/.codex/sessions/**/rollout-*.jsonl`). This reads them, read-only, per local day and model. Cached per file (a file is read again only when it changed), so the first look is the slow one.
//! Claude writes one line per content block with the same message id: counted once. Codex writes running totals: the difference between two readings is the call. Pure parsers, tested; `scan` walks the folders.
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Tokens of one model on one local day. `inp` is the uncached input, `cr` cache reads, `cw` cache writes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Use { pub inp: u64, pub out: u64, pub cr: u64, pub cw: u64, pub calls: u64 }
/// (day `YYYY-MM-DD`, model) → tokens.
pub type Days = BTreeMap<(String, String), Use>;

/// The local day of an ISO timestamp (`2026-11-16T12:56:54.377Z`) at `tz_min` minutes from UTC. Pure.
pub fn day_of_iso(ts: &str, tz_min: i64) -> Option<String> {
    let t = ts.get(..19)?; let y: i64 = t.get(0..4)?.parse().ok()?; let mo: i64 = t.get(5..7)?.parse().ok()?; let d: i64 = t.get(8..10)?.parse().ok()?; let h: i64 = t.get(11..13)?.parse().ok()?; let mi: i64 = t.get(14..16)?.parse().ok()?;
    let days = days_from_civil(y, mo, d); let total = days * 1440 + h * 60 + mi + tz_min; Some(civil_from_days(total.div_euclid(1440)))
}
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 { let y = if m <= 2 { y - 1 } else { y }; let era = y.div_euclid(400); let yoe = y.rem_euclid(400); let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468 }
fn civil_from_days(z: i64) -> String { let z = z + 719_468; let era = z.div_euclid(146_097); let doe = z.rem_euclid(146_097); let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; let y = yoe + era * 400; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153; let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 }; format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d) }
fn n(v: &Value) -> u64 { v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(0) }

/// One Claude Code transcript's text → tokens per day and model. A message id counts once. Pure.
pub fn parse_claude(text: &str, tz_min: i64) -> Days { parse_claude_lines(text.lines().map(String::from), tz_min) }
/// The same over a stream of lines (a transcript can be hundreds of MB: it is never held whole). Pure.
pub fn parse_claude_lines(lines: impl Iterator<Item = String>, tz_min: i64) -> Days { let mut r = Read::default(); for l in lines { claude_line(&l, tz_min, &mut r); } r.days }
/// How far a file has been read, and what was needed to go on from there: an appended file is read from `at`, never again from the start.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Read { pub at: u64, #[serde(with = "days_rows")] pub days: Days, /** the last message ids, so a message written over several lines counts once across two readings */ pub seen: std::collections::VecDeque<String>, pub model: String, pub prev: (u64, u64, u64) }
mod days_rows {
    use super::{Days, Use};
    pub fn serialize<S: serde::Serializer>(d: &Days, s: S) -> Result<S::Ok, S::Error> { serde::Serialize::serialize(&d.iter().map(|((a, m), u)| (a, m, u.inp, u.out, u.cr, u.cw, u.calls)).collect::<Vec<_>>(), s) }
    pub fn deserialize<'a, D: serde::Deserializer<'a>>(d: D) -> Result<Days, D::Error> { let v: Vec<(String, String, u64, u64, u64, u64, u64)> = serde::Deserialize::deserialize(d)?; Ok(v.into_iter().map(|(a, m, inp, out, cr, cw, calls)| ((a, m), Use { inp, out, cr, cw, calls })).collect()) }
}
const IDS_KEPT: usize = 400;
fn claude_line(l: &str, tz_min: i64, r: &mut Read) {
    if !l.contains("\"usage\"") || !l.contains("\"assistant\"") { return }
    let Ok(v) = serde_json::from_str::<Value>(l) else { return }; if v["type"] != "assistant" { return }
    let m = &v["message"]; let (Some(model), Some(ts)) = (m["model"].as_str(), v["timestamp"].as_str()) else { return }; if model.starts_with('<') || model == "synthetic" { return }
    if let Some(id) = m["id"].as_str() { if r.seen.iter().rev().any(|x| x == id) { return } r.seen.push_back(id.to_string()); if r.seen.len() > IDS_KEPT { r.seen.pop_front(); } }
    let Some(day) = day_of_iso(ts, tz_min) else { return }; let u = &m["usage"];
    let e = r.days.entry((day, model.to_string())).or_default(); e.inp += n(&u["input_tokens"]); e.out += n(&u["output_tokens"]); e.cr += n(&u["cache_read_input_tokens"]); e.cw += n(&u["cache_creation_input_tokens"]); e.calls += 1;
}
/// One Codex rollout's text → tokens per day and model: the difference between consecutive running totals is the call; the model is the one of the last `turn_context`. Pure.
pub fn parse_codex(text: &str, tz_min: i64) -> Days { parse_codex_lines(text.lines().map(String::from), tz_min) }
pub fn parse_codex_lines(lines: impl Iterator<Item = String>, tz_min: i64) -> Days { let mut r = Read::default(); for l in lines { codex_line(&l, tz_min, &mut r); } r.days }
fn codex_line(l: &str, tz_min: i64, r: &mut Read) {
    if r.model.is_empty() { r.model = "gpt-5".into(); }
    if l.contains("\"turn_context\"") { if let Ok(v) = serde_json::from_str::<Value>(l) { if let Some(m) = v["payload"]["model"].as_str() { r.model = m.to_string(); } } return }
    if !l.contains("\"token_count\"") { return }
    let Ok(v) = serde_json::from_str::<Value>(l) else { return }; let t = &v["payload"]["info"]["total_token_usage"]; if !t.is_object() { return }
    let (inp, cached, o) = (n(&t["input_tokens"]), n(&t["cached_input_tokens"]), n(&t["output_tokens"])); if (inp, cached, o) == r.prev || inp < r.prev.0 { return }
    let Some(day) = v["timestamp"].as_str().and_then(|ts| day_of_iso(ts, tz_min)) else { return };
    let (di, dc, dout) = (inp - r.prev.0, cached.saturating_sub(r.prev.1), o.saturating_sub(r.prev.2)); r.prev = (inp, cached, o);
    let e = r.days.entry((day, r.model.clone())).or_default(); e.inp += di.saturating_sub(dc); e.cr += dc; e.out += dout; e.calls += 1;
}
/// (file, time zone) → (its time, its length, how far it was read). ONE ENTRY PER ZONE (#26): the screen asks in the person's zone and an agent asked in UTC, and with one entry per file each of them read every changed file again from the start.
type Cache = HashMap<(PathBuf, i64), (std::time::SystemTime, u64, Read)>;
/// The zone the screen last asked in: what an agent that names none is answered in, so "today" is the person's day.
static SCREEN_TZ: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(i64::MIN);
pub fn screen_asked(tz_min: i64) { SCREEN_TZ.store(tz_min, std::sync::atomic::Ordering::Relaxed); }
pub fn screen_tz() -> Option<i64> { Some(SCREEN_TZ.load(std::sync::atomic::Ordering::Relaxed)).filter(|t| *t != i64::MIN) }
/// One file's days in one zone, reading only what is new for THAT zone; `true` when something was read. Blocking.
fn days_of(cache: &mut Cache, p: &Path, tz_min: i64, claude: bool, mt: std::time::SystemTime, len: u64) -> Option<(Days, bool)> {
    let key = (p.to_path_buf(), tz_min);
    if let Some((m, l, r)) = cache.get(&key) { if *m == mt && *l == len { return Some((r.days.clone(), false)); } }
    let from = cache.get(&key).map(|x| x.2.clone()).unwrap_or_default(); let r = read_on(p, tz_min, claude, from)?; let d = r.days.clone(); cache.insert(key, (mt, len, r)); Some((d, true))
}
/// Read what is new in a file: whole lines from `r.at`. A line still being written is left for the next time. Blocking.
pub fn read_on(path: &Path, tz_min: i64, claude: bool, mut r: Read) -> Option<Read> {
    use std::io::{BufRead, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?; if f.metadata().ok()?.len() < r.at { r = Read::default(); }   // rewritten: from the start
    f.seek(SeekFrom::Start(r.at)).ok()?; let mut reader = std::io::BufReader::new(f); let mut line = Vec::new();
    loop { line.clear(); let got = reader.read_until(b'\n', &mut line).ok()?; if got == 0 || line.last() != Some(&b'\n') { break; } r.at += got as u64; let text = String::from_utf8_lossy(&line); if claude { claude_line(text.trim_end(), tz_min, &mut r) } else { codex_line(text.trim_end(), tz_min, &mut r) } }
    Some(r)
}
/// KEPT ACROSS RESTARTS (found 2026-11-26: after every restart gigabytes of history were read again, a whole core for many minutes).
#[derive(serde::Serialize, serde::Deserialize)]
struct Kept { path: PathBuf, secs: u64, nanos: u32, len: u64, tz: i64, read: Read }
fn kept_file() -> Option<PathBuf> { if cfg!(test) { return None; } std::env::var_os("ANTILOKI_HOME").map(PathBuf::from).or_else(|| home().map(|h| h.join(".antiloki"))).map(|d| d.join("cli-usage-cache.json")) }
fn load_kept() -> Cache {
    let list: Vec<Kept> = kept_file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    list.into_iter().map(|k| ((k.path, k.tz), (std::time::UNIX_EPOCH + std::time::Duration::new(k.secs, k.nanos), k.len, k.read))).collect()
}
fn save_kept(c: &Cache) {
    let Some(f) = kept_file() else { return };
    let list: Vec<Kept> = c.iter().map(|((p, tz), (t, len, read))| { let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default(); Kept { path: p.clone(), secs: d.as_secs(), nanos: d.subsec_nanos(), len: *len, tz: *tz, read: read.clone() } }).collect();
    if let Ok(b) = serde_json::to_vec(&list) { let tmp = f.with_extension("tmp"); if std::fs::write(&tmp, b).is_ok() { let _ = std::fs::rename(tmp, f); } }
}
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
fn files_under(root: &Path, pred: &dyn Fn(&Path) -> bool, since: std::time::SystemTime, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(rd) = std::fs::read_dir(root) else { return }; for e in rd.flatten() { let p = e.path(); let Ok(ft) = e.file_type() else { continue }; if ft.is_symlink() { continue }
        if ft.is_dir() { if depth < 6 { files_under(&p, pred, since, out, depth + 1); } } else if pred(&p) && e.metadata().and_then(|m| m.modified()).map_or(false, |t| t >= since) { out.push(p); } }
}
fn home() -> Option<PathBuf> { std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from) }
/// Every Claude and Codex file touched in the last `days` days (and today for 0), as tokens per day and model; only the days inside the window are kept. Blocking: the first call reads, later calls re-read only what changed.
pub fn scan(days: i64, tz_min: i64, today: &str) -> Vec<(String, String, String, Use)> {
    let Some(h) = home() else { return vec![] }; let span = (days.max(1) + 1) as u64 * 86_400; let since = std::time::SystemTime::now() - std::time::Duration::from_secs(span);
    let mut files: Vec<(PathBuf, bool)> = vec![]; let mut a = vec![]; files_under(&h.join(".claude").join("projects"), &|p| p.extension().map_or(false, |x| x == "jsonl"), since, &mut a, 0); files.extend(a.into_iter().map(|p| (p, true)));
    let mut b = vec![]; files_under(&h.join(".codex").join("sessions"), &|p| p.file_name().and_then(|f| f.to_str()).map_or(false, |f| f.starts_with("rollout-") && f.ends_with(".jsonl")), since, &mut b, 0); files.extend(b.into_iter().map(|p| (p, false)));
    let mut all: Vec<(String, String, String, Use)> = vec![]; let first = days_back(today, if days > 0 { days - 1 } else { 0 }); let mut g = CACHE.lock().unwrap_or_else(|p| p.into_inner()); let cache = g.get_or_insert_with(load_kept);
    let mut changed = 0u32;
    for (p, claude) in files {
        let Ok(md) = std::fs::metadata(&p) else { continue }; let (mt, len) = (md.modified().unwrap_or(std::time::UNIX_EPOCH), md.len());
        let Some((d, read_now)) = days_of(cache, &p, tz_min, claude, mt, len) else { continue }; if read_now { changed += 1; if changed % 25 == 0 { save_kept(cache); } }
        for ((day, model), u) in d { if day.as_str() >= first.as_str() && day.as_str() <= today { all.push((day, model, if claude { "claude".into() } else { "codex".into() }, u)); } }
    }
    if changed > 0 { save_kept(cache); }
    all
}
fn days_back(day: &str, k: i64) -> String { let p: Vec<i64> = day.split('-').filter_map(|x| x.parse().ok()).collect(); if p.len() != 3 { return day.to_string() } civil_from_days(days_from_civil(p[0], p[1], p[2]) - k) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn asking_in_another_time_zone_never_makes_the_first_one_read_again() {
        let f = std::env::temp_dir().join(format!("antiloki-usage-tz-{}.jsonl", std::process::id()));
        std::fs::write(&f, "{\"type\":\"assistant\",\"timestamp\":\"2026-11-16T01:30:00.000Z\",\"message\":{\"id\":\"a\",\"model\":\"m\",\"usage\":{\"input_tokens\":2}}}\n").unwrap();
        let mt = f.metadata().unwrap().modified().unwrap(); let len = f.metadata().unwrap().len(); let mut c = Cache::new();
        let day = |d: &Days| d.keys().next().unwrap().0.clone();
        let (a, ra) = days_of(&mut c, &f, -180, true, mt, len).unwrap(); let (b, rb) = days_of(&mut c, &f, 0, true, mt, len).unwrap();
        assert_eq!((day(&a), day(&b), ra, rb), ("2026-11-15".into(), "2026-11-16".into(), true, true));
        assert!(!days_of(&mut c, &f, -180, true, mt, len).unwrap().1 && !days_of(&mut c, &f, 0, true, mt, len).unwrap().1, "both are kept"); let _ = std::fs::remove_file(f);
    }
    #[test] fn an_appended_file_is_read_only_from_where_it_was_left() {
        let f = std::env::temp_dir().join(format!("antiloki-usage-{}.jsonl", std::process::id()));
        let line = |id: &str| format!(r#"{{"type":"assistant","timestamp":"2026-11-16T12:00:00.000Z","message":{{"id":"{id}","model":"m","usage":{{"input_tokens":2,"output_tokens":3}}}}}}"#);
        std::fs::write(&f, format!("{}\n{}", line("a"), line("b"))).unwrap();   // "b" has no newline yet: it is being written
        let r = read_on(&f, 0, true, Read::default()).unwrap(); assert_eq!(r.days.values().map(|u| u.calls).sum::<u64>(), 1);
        std::fs::write(&f, format!("{}\n{}\n{}\n", line("a"), line("b"), line("b"))).unwrap();
        let r2 = read_on(&f, 0, true, r.clone()).unwrap(); assert_eq!(r2.days.values().map(|u| u.calls).sum::<u64>(), 2, "only what is new, and a repeated id once"); assert!(r2.at > r.at);
        let back: Read = serde_json::from_str(&serde_json::to_string(&r2).unwrap()).unwrap(); assert_eq!((back.at, back.days), (r2.at, r2.days.clone()));
        std::fs::write(&f, format!("{}\n", line("c"))).unwrap(); assert_eq!(read_on(&f, 0, true, r2).unwrap().days.values().map(|u| u.calls).sum::<u64>(), 1, "a rewritten file is read from the start"); let _ = std::fs::remove_file(f);
    }
    #[test] fn a_timestamp_is_the_local_day() { assert_eq!(day_of_iso("2026-11-16T12:56:54.377Z", 0).as_deref(), Some("2026-11-16")); assert_eq!(day_of_iso("2026-11-16T01:30:00.000Z", -180).as_deref(), Some("2026-11-15"), "01:30 UTC is 22:30 the day before at UTC-3"); assert_eq!(day_of_iso("2026-12-31T23:30:00.000Z", 60).as_deref(), Some("2027-01-01")); assert!(day_of_iso("x", 0).is_none()); }
    #[test] fn claude_counts_a_message_once_however_many_lines_it_was_written_in() {
        let l = |id: &str| format!(r#"{{"type":"assistant","timestamp":"2026-11-16T12:00:00.000Z","message":{{"id":"{id}","model":"claude-sonnet-5-5","usage":{{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":50}}}}}}"#);
        let text = format!("{}\n{}\n{}\n{{\"type\":\"user\"}}\n{{\"type\":\"assistant\",\"timestamp\":\"2026-11-16T12:00:00.000Z\",\"message\":{{\"id\":\"z\",\"model\":\"<synthetic>\",\"usage\":{{\"input_tokens\":9}}}}}}", l("a"), l("a"), l("b"));
        let d = parse_claude(&text, 0); let u = &d[&("2026-11-16".to_string(), "claude-sonnet-5-5".to_string())]; assert_eq!((u.inp, u.out, u.cr, u.cw, u.calls), (4, 100, 2000, 200, 2)); assert_eq!(d.len(), 1, "the synthetic model is not a model");
    }
    #[test] fn codex_calls_are_the_difference_between_running_totals_and_cached_input_is_not_charged_twice() {
        let tc = |ts: &str, i: u64, c: u64, o: u64| format!(r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{i},"cached_input_tokens":{c},"output_tokens":{o}}}}}}}}}"#);
        let text = format!("{{\"timestamp\":\"2026-11-16T10:00:00.000Z\",\"type\":\"turn_context\",\"payload\":{{\"model\":\"gpt-5.5\"}}}}\n{}\n{}\n{}\n{}", tc("2026-11-16T10:00:01.000Z", 1000, 0, 10), tc("2026-11-16T10:00:01.500Z", 1000, 0, 10), tc("2026-11-16T10:00:09.000Z", 3000, 1500, 60), "{\"type\":\"event_msg\"}");
        let d = parse_codex(&text, 0); let u = &d[&("2026-11-16".to_string(), "gpt-5.5".to_string())]; assert_eq!((u.inp, u.cr, u.out, u.calls), (1000 + 500, 1500, 60, 2), "a repeated reading adds nothing; 2000 new input of which 1500 cached");
    }
}
