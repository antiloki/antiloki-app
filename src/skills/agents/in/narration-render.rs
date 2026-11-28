//! narration-render — twin of skills/agents/in/narration-render.mjs, THE SPEECH RENDERER
//! chokepoint. NAMES ARE CONTRACT: `sectionize` (named, no-db, pure) + `narrationRender`
//! (default). Ops: render | sections | progress | render_get | renders | finalize.
//!
//! PARITY-DEFERRED: the actual speech synthesis (Kokoro-82M) is a real model inference call —
//! non-deterministic output (float32 PCM from a neural net), unfixturable in principle, same
//! class as ai-call.rs's `aiCall`. It is called via `crate::system_kokoro_speak::kokoroPcm`,
//! which does NOT exist in this crate yet (no Rust twin has been authored for
//! skills/system/kokoro-speak.mjs) — this file references it per the sanctioned "PARITY-DEFERRED
//! boundary" convention (see ai-call.rs / data-story-propose.rs) and will not compile until that
//! sibling stub lands with signature `pub fn kokoroPcm(input: &Value, db: &Connection) ->
//! Result<Value, String>` returning `Err(...)` unconditionally (mirroring `aiCall`'s stub), or
//! `Ok(json!({"samples": [f64...], "rate": u32}))` in a future real backend. RATE/DEFAULT_VOICE/
//! BACKEND_VERSION are duplicated here from the JS module's pinned constants (no Rust source to
//! import them from yet) — if/when a real kokoro-speak.rs twin is authored, these three consts
//! should be deleted here and re-pointed at the twin's own pub consts.
//!
//! Everything AROUND the deferred call is ported faithfully and is real, reachable, testable
//! code: op routing, sectionize's derive-from-IR algorithm, db reads/writes for every op, the
//! events-library clip lookup (real WAV file decode), procedural breath / silence assembly, the
//! 1h replay-cache TTL sweep, finalize's shelf artifact assembly (concat + wav encode + sidecar
//! json), and the `press()` control flow for every unit kind OTHER than `say` (pause/breath/
//! event/unknown never touch the deferred dep at all — a narration with no `say` units presses
//! fully deterministically end to end, DB row and wav file included).
// QUALITY: full op-routing + sectionize + fs/db assembly ported; the one non-deterministic leaf
// (kokoroPcm) is a PARITY-DEFERRED boundary, not yet backed by a Rust module in this crate.

use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::ErrorKind;

use crate::engine_roots::{data_dir, sources_dir};
use crate::wav_tools;

// ── local JS-bridge helpers (each twin defines its own — see narration-compile.rs) ──
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn js_round(x: f64) -> f64 { (x + 0.5).floor() } // JS Math.round semantics (ties toward +Inf)

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let total_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let days = total_ms.div_euclid(86_400_000);
    let ms_of_day = total_ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let hour = ms_of_day / 3_600_000;
    let minute = (ms_of_day / 60_000) % 60;
    let second = (ms_of_day / 1000) % 60;
    let millis = ms_of_day % 1000;
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

// ── skills/system/kokoro-speak.mjs pinned constants, duplicated (see module doc) ──
pub const RATE: u32 = 24000;
pub const DEFAULT_VOICE: &str = "af_heart";
pub const BACKEND_VERSION: &str = "kokoro-82M-v1.0/q8";

const BREATH_SHORT_MS: f64 = 350.0;
const BREATH_LONG_MS: f64 = 650.0;
const MAX_SAYS: usize = 240;
const TTL_MS: u128 = 60 * 60 * 1000;

fn events_dir() -> std::path::PathBuf { data_dir().join("narration-events") }
fn out_dir() -> std::path::PathBuf { std::env::temp_dir().join("antiloki-narrations") }

fn sweep_expired() {
    let dir = out_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if let Ok(meta) = std::fs::metadata(&p) {
            if let Ok(modified) = meta.modified() {
                if let Ok(age) = std::time::SystemTime::now().duration_since(modified) {
                    if age.as_millis() > TTL_MS {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        }
    }
}

fn cached_wav(path: &str) -> bool {
    if path.is_empty() { return false; }
    match std::fs::metadata(path) {
        Ok(meta) => match meta.modified() {
            Ok(modified) => std::time::SystemTime::now().duration_since(modified).map(|age| age.as_millis() <= TTL_MS).unwrap_or(false),
            Err(_) => false,
        },
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(_) => false,
    }
}

fn event_clip(kind: &str, rate: u32) -> Option<Vec<f32>> {
    let p = events_dir().join(format!("{kind}.wav"));
    let bytes = std::fs::read(&p).ok()?;
    let parsed = wav_tools::decodeWav16(&bytes)?;
    Some(wav_tools::resampleNearest(&parsed.samples, parsed.rate, rate))
}

// ── PRESS PROGRESS — persisted, deliberately NOT process-local ──────────────────────────────
// This replaced a module-level `Mutex<HashMap>` (the JS `PROGRESS = new Map()` twin). That map
// lived in the addon's statics, so ONLY the thread running the press could read it. Harmless while
// the press ran inline on the request thread; fatal the moment it moves to a worker, because
// op:"progress" would then read a different, empty map on the request path and report a press that
// never advances. A row is visible to any connection — WAL gives the request thread concurrent
// reads while the worker writes — which is what makes a detached press observable at all.
//
// The JSON shape handed back is byte-for-byte what the map produced (done/total/started_at, plus
// render_id or error ONLY once they exist, which is exactly what the `pressing` test below reads),
// so op:"progress" and the FE are unchanged. Only the storage moved.
fn now_ms_f64() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as f64
}

fn progress_begin(db: &Connection, key: &str, narration_id: i64, section_ord: Option<i64>, total: i64) -> Result<(), String> {
    let now = now_ms_f64();
    // INSERT OR REPLACE, so re-pressing a section wipes the previous attempt's row: a stale job can
    // never be mistaken for the live one, which an append-only log would have allowed.
    crate::db_json::run_write(
        db,
        "INSERT OR REPLACE INTO knowledge.narration_press_progress
           (progress_key, narration_id, section_ord, status, done, total, render_id, error, started_at, updated_at)
         VALUES (?, ?, ?, 'pressing', 0, ?, NULL, NULL, ?, ?)",
        &[
            json!(key), json!(narration_id), section_ord.map(Value::from).unwrap_or(Value::Null),
            json!(total), json!(now), json!(now),
        ],
    )?;
    Ok(())
}

// Per-unit tick. Best-effort by design: telemetry must never fail the press it is describing, and
// the UPDATE no-ops when there is no row — matching the old map, which only wrote if the key existed.
fn progress_tick(db: &Connection, key: &str, done: i64, total: i64) {
    let _ = crate::db_json::run_write(
        db,
        "UPDATE knowledge.narration_press_progress SET done = ?, total = ?, updated_at = ? WHERE progress_key = ?",
        &[json!(done), json!(total), json!(now_ms_f64()), json!(key)],
    );
}

fn progress_done(db: &Connection, key: &str, done: i64, render_id: Value) {
    let _ = crate::db_json::run_write(
        db,
        "UPDATE knowledge.narration_press_progress SET status = 'done', done = ?, render_id = ?, updated_at = ? WHERE progress_key = ?",
        &[json!(done), render_id, json!(now_ms_f64()), json!(key)],
    );
}

fn progress_error(db: &Connection, key: &str, err: &str) {
    let _ = crate::db_json::run_write(
        db,
        "UPDATE knowledge.narration_press_progress SET status = 'error', error = ?, updated_at = ? WHERE progress_key = ?",
        &[json!(err), json!(now_ms_f64()), json!(key)],
    );
}

/// The row shaped as the in-memory map used to hand it back — `None` when no press was ever
/// registered for this key, which is the "no entry" case the caller's `pressing` test depends on.
fn progress_read(db: &Connection, key: &str) -> Option<Value> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT done, total, started_at, render_id, error FROM knowledge.narration_press_progress WHERE progress_key = ?",
        &[json!(key)],
    ).ok()?;
    let row = rows.as_array()?.first()?.as_object()?.clone();
    let mut out = serde_json::Map::new();
    out.insert("done".into(), row.get("done").cloned().unwrap_or(json!(0)));
    out.insert("total".into(), row.get("total").cloned().unwrap_or(json!(0)));
    out.insert("started_at".into(), row.get("started_at").cloned().unwrap_or(Value::Null));
    // Present ONLY when set — absence is what marks a press as still in flight.
    if let Some(r) = row.get("render_id").filter(|v| !v.is_null()) { out.insert("render_id".into(), r.clone()); }
    if let Some(e) = row.get("error").filter(|v| !v.is_null()) { out.insert("error".into(), e.clone()); }
    Some(Value::Object(out))
}

/// `export function sectionize(ir)` — sections are DERIVED from the IR: every `say` carries its
/// `receipt.sec`; a say whose sec differs from the previous say's opens a new section. Non-say
/// units ride with the section in progress.
pub fn sectionize(ir: &Value) -> Value {
    let units = ir.get("units").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut sections: Vec<Value> = Vec::new();
    let mut cur_idx: Option<usize> = None;
    let mut last_sec: Option<String> = None;
    for i in 0..units.len() {
        let u = &units[i];
        if u.get("t").and_then(Value::as_str) == Some("say") {
            // JS: String(u.receipt?.sec ?? '') — null/undefined receipt.sec collapses to ''.
            let sec = match u.get("receipt").and_then(|r| nz(r.get("sec"))) {
                Some(v) => js_string(v),
                None => String::new(),
            };
            if cur_idx.is_none() || Some(&sec) != last_sec.as_ref() {
                if let Some(ci) = cur_idx {
                    sections[ci]["to"] = json!(i as i64 - 1);
                }
                let ord = sections.len() as i64;
                let title = if sec.is_empty() { "opening".to_string() } else { sec.clone() };
                let from = if cur_idx.is_none() { 0i64 } else { i as i64 };
                sections.push(json!({ "ord": ord, "title": title, "from": from, "to": i as i64 }));
                cur_idx = Some(sections.len() - 1);
            }
            last_sec = Some(sec);
        }
    }
    if let Some(ci) = cur_idx {
        sections[ci]["to"] = json!(units.len() as i64 - 1);
    }
    json!(sections)
}

fn press(
    db: &Connection,
    nrow_id: i64,
    section_ord: Option<i64>,
    section_title: Option<&str>,
    base_index: i64, // JS: section?.from ?? 0 — absolute-unit-index base for the timing array
    unit_slice: &[Value],
    ir_voices: &Value,
    voice: &str,
    progress_key: &str,
    track_progress: bool,
) -> Result<Value, String> {
    let mut requested: Vec<String> = Vec::new();
    let mut honored: Vec<String> = Vec::new();
    let mut degraded: Vec<Value> = Vec::new();
    let mut chunks: Vec<Vec<f32>> = Vec::new();
    let mut timing: Vec<Value> = Vec::new();
    let mut cursor: usize = 0;
    let push_str = |v: &mut Vec<String>, s: String| if !v.contains(&s) { v.push(s); };

    for (i, u) in unit_slice.iter().enumerate() {
        if track_progress {
            progress_tick(db, progress_key, i as i64, unit_slice.len() as i64);
        }
        let t = u.get("t").and_then(Value::as_str).unwrap_or("");
        if t == "say" {
            push_str(&mut requested, "say".to_string());
            let voice_role = u.get("voice_role").and_then(Value::as_str);
            let v = match voice_role {
                Some(role) if ir_voices.get(role).map(|x| !x.is_null()).unwrap_or(false) => js_string(ir_voices.get(role).unwrap()),
                _ => voice.to_string(),
            };
            let text = str_qq(u.get("text"));
            let ai_input = json!({ "text": text, "voice": v });
            let (samples, rate) = crate::system_kokoro_speak::kokoroPcm(&ai_input, &Value::Null)?;
            let pcm_chunk = if rate == RATE { samples } else { wav_tools::resampleNearest(&samples, rate, RATE) };
            let abs_i = base_index + i as i64; // JS: (section?.from ?? 0) + i — ABSOLUTE unit index
            timing.push(json!({
                "i": abs_i,
                "role": voice_role,
                "t": js_round(cursor as f64 / RATE as f64 * 1000.0) as i64,
                "d": js_round(pcm_chunk.len() as f64 / RATE as f64 * 1000.0) as i64,
            }));
            cursor += pcm_chunk.len();
            chunks.push(pcm_chunk);
            push_str(&mut honored, if voice_role.is_some() { format!("say:{}", voice_role.unwrap()) } else { "say".to_string() });
            continue;
        }
        if t == "pause" {
            push_str(&mut requested, "pause".to_string());
            let ms = u.get("ms").and_then(Value::as_f64).unwrap_or(0.0);
            let chunk = wav_tools::silence(ms, RATE);
            cursor += chunk.len();
            chunks.push(chunk);
            push_str(&mut honored, "pause".to_string());
            continue;
        }
        if t == "breath" {
            let kind = u.get("kind").and_then(Value::as_str).unwrap_or("");
            push_str(&mut requested, format!("breath:{kind}"));
            let clip = event_clip(&format!("breath_{kind}"), RATE);
            let chunk = clip.clone().unwrap_or_else(|| wav_tools::proceduralBreath(kind, RATE));
            cursor += chunk.len();
            chunks.push(chunk);
            push_str(&mut honored, if clip.is_some() { format!("breath:{kind}:clip") } else { format!("breath:{kind}:procedural-v2") });
            continue;
        }
        if t == "event" {
            let kind = u.get("kind").and_then(Value::as_str).unwrap_or("");
            push_str(&mut requested, format!("event:{kind}"));
            if kind == "code_omitted" || kind == "table_omitted" {
                push_str(&mut honored, format!("event:{kind}:silent"));
                continue;
            }
            match event_clip(kind, RATE) {
                Some(chunk) => {
                    cursor += chunk.len();
                    chunks.push(chunk);
                    push_str(&mut honored, format!("event:{kind}:clip"));
                }
                None => degraded.push(json!({ "unit": i as i64, "kind": kind, "reason": "no clip in the events library — dropped" })),
            }
            continue;
        }
        degraded.push(json!({ "unit": i as i64, "kind": t, "reason": "unknown unit kind for this renderer version — ignored (nir forward-compat)" }));
    }

    let pcm = wav_tools::normalize(&wav_tools::concatF32(&chunks), 0.9);
    let wav = wav_tools::encodeWav16(&pcm, RATE);
    let duration_ms = js_round(pcm.len() as f64 / RATE as f64 * 1000.0) as i64;
    let waveform_hash = hex::encode(Sha256::digest(&wav))[..32].to_string();

    std::fs::create_dir_all(out_dir()).map_err(|e| e.to_string())?;
    sweep_expired();
    let section_ord_v = section_ord.map(Value::from).unwrap_or(Value::Null);
    crate::db_json::run_write(
        db,
        "INSERT INTO knowledge.narration_renders
           (narration_id, section_ord, section_title, backend, backend_version, voice, waveform_path, waveform_hash, duration_ms,
            features_requested_json, features_honored_json, degraded_json, timing_json, cost_usd)
         VALUES (?, ?, ?, 'kokoro', ?, ?, '', ?, ?, ?, ?, ?, ?, 0)",
        &[
            json!(nrow_id), section_ord_v.clone(), section_title.map(Value::from).unwrap_or(Value::Null),
            json!(BACKEND_VERSION), json!(voice), json!(waveform_hash), json!(duration_ms),
            json!(requested), json!(honored), json!(degraded), json!(timing),
        ],
    )?;
    let render_id = db.last_insert_rowid();
    let abs = out_dir().join(format!("render-{render_id}.wav"));
    std::fs::write(&abs, &wav).map_err(|e| e.to_string())?;
    let abs_str = abs.to_string_lossy().to_string();
    crate::db_json::run_write(db, "UPDATE knowledge.narration_renders SET waveform_path = ? WHERE id = ?", &[json!(abs_str), json!(render_id)])?;

    Ok(json!({
        "ok": true,
        "render_id": render_id,
        "narration_id": nrow_id,
        "section_ord": section_ord_v,
        "section_title": section_title,
        "backend": "kokoro",
        "backend_version": BACKEND_VERSION,
        "voice": voice,
        "duration_ms": duration_ms,
        "waveform_hash": waveform_hash,
        "features_requested": requested,
        "features_honored": honored,
        "degraded": degraded,
        "cost_usd": 0,
    }))
}

/// `export default async function narrationRender(input, context)`. `context` collapses to `db`
/// (the JS `context.say_fn` test seam becomes the PARITY-DEFERRED `system_kokoro_speak::kokoroPcm`
/// boundary — see module doc).
pub fn narrationRender(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = match nz(input.get("op")) { Some(v) => js_string(v), None => "render".into() };

    if op == "render_get" {
        let render_id_num = js_number(input.get("render_id"));
        let param = if render_id_num.is_finite() { json!(render_id_num) } else { Value::Null };
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.narration_renders WHERE id = ?", &[param])?;
        let Some(row) = rows.as_array().and_then(|a| a.first()).cloned() else {
            return Err(format!("narration-render: render '{}' not found", tmpl(input.get("render_id"))));
        };
        let waveform_path = row.get("waveform_path").and_then(Value::as_str).unwrap_or("");
        if !cached_wav(waveform_path) {
            return Err(format!("narration-render: cached wav (1h TTL expired — press again; the receipt row remains) '{}' not found", tmpl(input.get("render_id"))));
        }
        return Ok(json!({ "ok": true, "render": row, "abs_path": waveform_path }));
    }

    if op == "finalize" {
        let narration_id_num = js_number(input.get("narration_id"));
        let param = if narration_id_num.is_finite() { json!(narration_id_num) } else { Value::Null };
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.narrations WHERE id = ?", &[param])?;
        let Some(nrow2) = rows.as_array().and_then(|a| a.first()).cloned() else {
            return Err(format!("narration-render: narration '{}' not found", tmpl(input.get("narration_id"))));
        };
        let nrow2_id = nrow2.get("id").and_then(Value::as_i64).unwrap_or(0);
        let ir_json_str = nrow2.get("ir_json").and_then(Value::as_str).unwrap_or("null");
        let ir2: Value = serde_json::from_str(ir_json_str).unwrap_or(Value::Null);
        let secs = sectionize(&ir2);
        let secs_arr = secs.as_array().cloned().unwrap_or_default();
        let mut parts: Vec<Vec<f32>> = Vec::new();
        let mut missing: Vec<i64> = Vec::new();
        for s in &secs_arr {
            let ord = s.get("ord").and_then(Value::as_i64).unwrap_or(0);
            let rows2 = crate::db_json::query_rows(
                db,
                "SELECT * FROM knowledge.narration_renders WHERE narration_id = ? AND section_ord = ? ORDER BY id DESC LIMIT 1",
                &[json!(nrow2_id), json!(ord)],
            )?;
            let Some(row) = rows2.as_array().and_then(|a| a.first()).cloned() else { missing.push(ord); continue; };
            let waveform_path = row.get("waveform_path").and_then(Value::as_str).unwrap_or("").to_string();
            if !cached_wav(&waveform_path) { missing.push(ord); continue; }
            let Ok(bytes) = std::fs::read(&waveform_path) else { missing.push(ord); continue; };
            let Some(parsed) = wav_tools::decodeWav16(&bytes) else { missing.push(ord); continue; };
            parts.push(parsed.samples);
        }
        if !missing.is_empty() {
            let list = missing.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
            return Err(format!("narration-render: all sections pressed and cached to finalize (missing ords: {list}) — press them, then finalize"));
        }
        let pcm = wav_tools::concatF32(&parts);
        let wav = wav_tools::encodeWav16(&pcm, RATE);
        let kind_default = if nrow2.get("source_kind").and_then(Value::as_str) == Some("podcast") { "podcast" } else { "narration" };
        let kind_raw = match nz(input.get("kind")) { Some(v) => js_string(v), None => kind_default.to_string() };
        let kind_re = regex::Regex::new(r"(?i)[^a-z0-9-]").unwrap();
        let kind: String = kind_re.replace_all(&kind_raw, "-").chars().take(24).collect();
        let artefacts = sources_dir().join("artefacts");
        std::fs::create_dir_all(&artefacts).map_err(|e| e.to_string())?;
        let ir_hash = nrow2.get("ir_hash").and_then(Value::as_str).unwrap_or("");
        let base = format!("{kind}-n{nrow2_id}-{}", &ir_hash[..ir_hash.len().min(8)]);
        let file = format!("{base}.wav");
        std::fs::write(artefacts.join(&file), &wav).map_err(|e| e.to_string())?;
        let sess_rows = crate::db_json::query_rows(db, "SELECT id FROM knowledge.reasoning_sessions WHERE narration_id = ? ORDER BY id DESC LIMIT 1", &[json!(nrow2_id)])?;
        let session_id = sess_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);
        let first_render_rows = crate::db_json::query_rows(db, "SELECT voice FROM knowledge.narration_renders WHERE narration_id = ? ORDER BY id DESC LIMIT 1", &[json!(nrow2_id)])?;
        let voice = first_render_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("voice")).cloned().unwrap_or(Value::Null);
        let title: String = match nz(input.get("title")) { Some(v) => js_string(v), None => nrow2.get("title").and_then(Value::as_str).unwrap_or("").to_string() }
            .chars().take(200).collect();
        let deck = match input.get("deck") {
            Some(d) if !d.is_null() => {
                let s = serde_json::to_string(d).unwrap_or_default();
                if s.chars().count() < 300000 { d.clone() } else { Value::Null }
            }
            _ => Value::Null,
        };
        let duration_ms = js_round(pcm.len() as f64 / RATE as f64 * 1000.0) as i64;
        let meta = json!({
            "v": 1,
            "kind": kind,
            "title": title,
            "file": file,
            "narration_id": nrow2_id,
            "session_id": session_id,
            "voice": voice,
            "deck": deck,
            "source_kind": nrow2.get("source_kind").cloned().unwrap_or(Value::Null),
            "source_key": nrow2.get("source_key").cloned().unwrap_or(Value::Null),
            "ir_hash": ir_hash,
            "voices": ir2.get("voices").cloned().unwrap_or(Value::Null),
            "sections": secs_arr.len() as i64,
            "duration_ms": duration_ms,
            "bytes": wav.len() as i64,
            "backend_version": BACKEND_VERSION,
            "created_at": now_iso(),
        });
        std::fs::write(artefacts.join(format!("{base}.json")), serde_json::to_string_pretty(&meta).unwrap_or_default()).map_err(|e| e.to_string())?;
        let mut out = meta.as_object().cloned().unwrap_or_default();
        out.insert("ok".into(), json!(true));
        out.insert("abs_path".into(), json!(artefacts.join(&file).to_string_lossy().to_string()));
        return Ok(Value::Object(out));
    }

    if op == "renders" {
        let narration_id_num = js_number(input.get("narration_id"));
        let param = if narration_id_num.is_finite() { json!(narration_id_num) } else { Value::Null };
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.narration_renders WHERE narration_id = ? ORDER BY id DESC LIMIT 20", &[param])?;
        return Ok(json!({ "ok": true, "renders": rows }));
    }

    if op == "progress" {
        let nid = js_number(input.get("narration_id"));
        let nid_i = if nid.is_finite() { nid as i64 } else { 0 };
        let skey = match input.get("section_ord") {
            Some(v) if !v.is_null() => format!("{nid_i}:{}", js_number(Some(v)) as i64),
            _ => format!("{nid_i}:all"),
        };
        let p = progress_read(db, &skey);
        let last = match input.get("section_ord") {
            Some(v) if !v.is_null() => crate::db_json::query_rows(
                db, "SELECT id FROM knowledge.narration_renders WHERE narration_id = ? AND section_ord = ? ORDER BY id DESC LIMIT 1",
                &[json!(nid_i), json!(js_number(Some(v)) as i64)],
            )?,
            _ => crate::db_json::query_rows(db, "SELECT id FROM knowledge.narration_renders WHERE narration_id = ? ORDER BY id DESC LIMIT 1", &[json!(nid_i)])?,
        };
        let last_id = last.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);
        let pressing = p.is_some()
            && p.as_ref().and_then(|v| v.get("error")).is_none()
            && p.as_ref().and_then(|v| v.get("render_id")).map(|v| v.is_null()).unwrap_or(true);
        let mut out = p.and_then(|v| v.as_object().cloned()).unwrap_or_default();
        out.insert("ok".into(), json!(true));
        out.insert("pressing".into(), json!(pressing));
        out.insert("last_render_id".into(), last_id);
        return Ok(Value::Object(out));
    }

    // press_run shares every derivation below with render (narration row → sections → unit slice),
    // so it rides the same path and splits off at the end. It is the EXECUTION half of a background
    // render: the caller that accepted the job runs it, off the request thread.
    if op != "render" && op != "sections" && op != "press_run" {
        return Err("narration-render: op 'render'|'sections'|'press_run'|'progress'|'render_get'|'renders'|'finalize' is required".into());
    }

    let narration_id_num = js_number(input.get("narration_id"));
    let param = if narration_id_num.is_finite() { json!(narration_id_num) } else { Value::Null };
    let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.narrations WHERE id = ?", &[param])?;
    let Some(nrow) = rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("narration-render: narration '{}' not found", tmpl(input.get("narration_id"))));
    };
    let nrow_id = nrow.get("id").and_then(Value::as_i64).unwrap_or(0);
    let ir_json_str = nrow.get("ir_json").and_then(Value::as_str).unwrap_or("null");
    let ir: Value = serde_json::from_str(ir_json_str).unwrap_or(Value::Null);
    let voice = match nz(input.get("voice")) { Some(v) => js_string(v), None => DEFAULT_VOICE.to_string() };

    let all_sections = sectionize(&ir);
    let all_sections_arr = all_sections.as_array().cloned().unwrap_or_default();

    if op == "sections" {
        let units = ir.get("units").and_then(Value::as_array).cloned().unwrap_or_default();
        let est = |from: usize, to: usize| -> (i64, i64) {
            let mut ms = 0i64;
            let mut tokens = 0i64;
            for i in from..=to.min(units.len().saturating_sub(1)) {
                if i >= units.len() { break; }
                let u = &units[i];
                match u.get("t").and_then(Value::as_str) {
                    Some("say") => {
                        let text = str_qq(u.get("text"));
                        let words = text.split_whitespace().filter(|w| !w.is_empty()).count() as f64;
                        ms += js_round(words * 60000.0 / 155.0) as i64;
                        tokens += (text.chars().count() as f64 / 4.0).ceil() as i64;
                    }
                    Some("pause") => ms += u.get("ms").and_then(Value::as_i64).unwrap_or(0),
                    Some("breath") => {
                        let kind = u.get("kind").and_then(Value::as_str).unwrap_or("");
                        ms += if kind == "long" { BREATH_LONG_MS } else { BREATH_SHORT_MS } as i64;
                    }
                    _ => {}
                }
            }
            (ms, tokens)
        };
        let mut sections_out: Vec<Value> = Vec::new();
        for s in &all_sections_arr {
            let ord = s.get("ord").and_then(Value::as_i64).unwrap_or(0);
            let from = s.get("from").and_then(Value::as_i64).unwrap_or(0) as usize;
            let to = s.get("to").and_then(Value::as_i64).unwrap_or(0) as usize;
            let says = if to >= from && to < units.len() {
                units[from..=to].iter().filter(|u| u.get("t").and_then(Value::as_str) == Some("say")).count() as i64
            } else { 0 };
            let (est_ms, tokens_est) = if to >= from { est(from, to) } else { (0, 0) };
            let r_rows = crate::db_json::query_rows(
                db,
                "SELECT id, waveform_path, duration_ms, timing_json FROM knowledge.narration_renders WHERE narration_id = ? AND section_ord = ? ORDER BY id DESC LIMIT 1",
                &[json!(nrow_id), json!(ord)],
            )?;
            let r = r_rows.as_array().and_then(|a| a.first()).cloned();
            let timing: Value = r.as_ref()
                .and_then(|row| row.get("timing_json"))
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or_else(|| json!([]));
            let waveform_path = r.as_ref().and_then(|row| row.get("waveform_path")).and_then(Value::as_str).unwrap_or("");
            let render_id = if r.is_some() && cached_wav(waveform_path) { r.as_ref().and_then(|row| row.get("id")).cloned().unwrap_or(Value::Null) } else { Value::Null };
            let duration_ms = r.as_ref().and_then(|row| row.get("duration_ms")).cloned().unwrap_or(Value::Null);
            sections_out.push(json!({
                "ord": ord, "title": s.get("title").cloned().unwrap_or(Value::Null),
                "units": to as i64 - from as i64 + 1, "says": says,
                "est_ms": est_ms, "tokens_est": tokens_est,
                "render_id": render_id, "duration_ms": duration_ms, "timing": timing,
            }));
        }
        return Ok(json!({
            "ok": true, "narration_id": nrow_id, "title": nrow.get("title").cloned().unwrap_or(Value::Null),
            "sections": sections_out,
        }));
    }

    let section_ord: Option<i64> = match input.get("section_ord") {
        Some(v) if !v.is_null() => Some(js_number(Some(v)) as i64),
        _ => None,
    };
    let section = section_ord.and_then(|ord| all_sections_arr.iter().find(|s| s.get("ord").and_then(Value::as_i64) == Some(ord)));
    if section_ord.is_some() && section.is_none() {
        return Err(format!("narration-render: section '{}' not found", section_ord.unwrap()));
    }
    let units = ir.get("units").and_then(Value::as_array).cloned().unwrap_or_default();
    let unit_slice: Vec<Value> = match section {
        Some(s) => {
            let from = s.get("from").and_then(Value::as_i64).unwrap_or(0) as usize;
            let to = s.get("to").and_then(Value::as_i64).unwrap_or(0) as usize;
            if to >= from && to < units.len() { units[from..=to].to_vec() } else { Vec::new() }
        }
        None => units.clone(),
    };
    let says_count = unit_slice.iter().filter(|u| u.get("t").and_then(Value::as_str) == Some("say")).count();
    if says_count > MAX_SAYS {
        return Err(format!("narration-render: a narration under {MAX_SAYS} say-units (this one has {says_count}) — press it per section instead"));
    }
    let progress_key = match section_ord { Some(o) => format!("{nrow_id}:{o}"), None => format!("{nrow_id}:all") };
    let section_title = section.and_then(|s| s.get("title")).and_then(Value::as_str);
    let base_index = section.and_then(|s| s.get("from")).and_then(Value::as_i64).unwrap_or(0);
    let ir_voices = ir.get("voices").cloned().unwrap_or(Value::Null);

    if input.get("background").and_then(Value::as_bool).unwrap_or(false) {
        // Adopt an in-flight press rather than starting a second one over the same staging file.
        // Now survives a BE restart too: the row outlives the process the old map died with.
        if let Some(inflight) = progress_read(db, &progress_key) {
            let no_error = inflight.get("error").is_none();
            let no_render = inflight.get("render_id").map(|v| v.is_null()).unwrap_or(true);
            let started_at = inflight.get("started_at").and_then(Value::as_f64).unwrap_or(0.0);
            if no_error && no_render && (now_ms_f64() - started_at) < 30.0 * 60000.0 {
                let total = inflight.get("total").cloned().unwrap_or(Value::Null);
                return Ok(json!({ "ok": true, "started": true, "already": true, "total_units": total }));
            }
        }
        // ACCEPTED, NOT EXECUTED. `background: true` used to press right here and answer only once
        // the audio existed — a progress-shaped envelope around a synchronous call, which is the
        // whole reason a 30s client budget reported "could not start" for sections that were being
        // made, and why a 50s press froze every other request on the single-threaded engine. It now
        // means what it says: validate, register the job, return. The caller runs op:"press_run"
        // off the request thread; progress is already in the db for anyone to read meanwhile.
        //
        // Registration happens HERE, synchronously, on purpose. If the row appeared only once the
        // worker got going, a poll landing in that gap would see no press in flight and take
        // `last_render_id` — silently adopting a PREVIOUS render of the section as this one's.
        progress_begin(db, &progress_key, nrow_id, section_ord, unit_slice.len() as i64)?;
        return Ok(json!({
            "ok": true, "started": true, "total_units": unit_slice.len() as i64,
            "est_says": says_count as i64, "section_ord": section_ord,
        }));
    }

    if op == "press_run" {
        // The execution half. Runs on whatever thread the caller gives it — the BE hands this to a
        // single-slot writer lane so it never sits on the request thread. Progress was registered by
        // the accepting call, so this only advances and closes it.
        return match press(db, nrow_id, section_ord, section_title, base_index, &unit_slice, &ir_voices, &voice, &progress_key, true) {
            Ok(r) => {
                let rid = r.get("render_id").cloned().unwrap_or(Value::Null);
                progress_done(db, &progress_key, unit_slice.len() as i64, rid.clone());
                Ok(json!({ "ok": true, "render_id": rid, "section_ord": section_ord }))
            }
            // The failure is reported through progress, not thrown: the client is polling, not
            // holding this call, so an Err here would only be seen by the lane that ran it.
            Err(e) => {
                let msg: String = e.chars().take(200).collect();
                progress_error(db, &progress_key, &msg);
                Ok(json!({ "ok": false, "error": msg, "section_ord": section_ord }))
            }
        };
    }

    press(db, nrow_id, section_ord, section_title, base_index, &unit_slice, &ir_voices, &voice, &progress_key, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
            CREATE TABLE knowledge.narrations (id INTEGER PRIMARY KEY, source_kind TEXT NOT NULL, source_key TEXT NOT NULL, title TEXT NOT NULL, ir_version TEXT NOT NULL DEFAULT 'nir-1', ir_json TEXT NOT NULL, ir_hash TEXT NOT NULL, units_count INTEGER NOT NULL, words INTEGER NOT NULL DEFAULT 0, duration_est_ms INTEGER NOT NULL DEFAULT 0, created_at TEXT);
            CREATE TABLE knowledge.narration_renders (id INTEGER PRIMARY KEY, narration_id INTEGER NOT NULL, section_ord INTEGER, section_title TEXT, backend TEXT NOT NULL, backend_version TEXT, voice TEXT, waveform_path TEXT, waveform_hash TEXT, duration_ms INTEGER, features_requested_json TEXT NOT NULL DEFAULT '[]', features_honored_json TEXT NOT NULL DEFAULT '[]', degraded_json TEXT NOT NULL DEFAULT '[]', timing_json TEXT, cost_usd REAL NOT NULL DEFAULT 0, created_at TEXT);
            CREATE TABLE knowledge.reasoning_sessions (id INTEGER PRIMARY KEY, narration_id INTEGER);
            CREATE TABLE knowledge.narration_press_progress (progress_key TEXT PRIMARY KEY, narration_id INTEGER NOT NULL, section_ord INTEGER, status TEXT NOT NULL DEFAULT 'pressing', done INTEGER NOT NULL DEFAULT 0, total INTEGER NOT NULL DEFAULT 0, render_id INTEGER, error TEXT, started_at REAL NOT NULL, updated_at REAL NOT NULL);",
        ).unwrap();
        c
    }

    fn ir_no_say(sec_titles: &[&str]) -> Value {
        // a narration made only of pause/breath/event units never touches the deferred kokoro
        // dependency at all — this is a REAL reachable path, not a stub-only fixture.
        let mut units = Vec::new();
        for t in sec_titles {
            units.push(json!({ "t": "pause", "ms": 200 }));
            units.push(json!({ "t": "breath", "kind": "short" }));
            let _ = t;
        }
        units.push(json!({ "t": "event", "kind": "code_omitted" }));
        json!({ "v": "nir-1", "units": units })
    }

    fn insert_narration(c: &Connection, ir: &Value) -> i64 {
        let ir_json = serde_json::to_string(ir).unwrap();
        c.execute(
            "INSERT INTO knowledge.narrations (source_kind, source_key, title, ir_json, ir_hash, units_count) VALUES ('text','k','T',?,?,1)",
            rusqlite::params![ir_json, "hash1"],
        ).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn sectionize_groups_by_receipt_sec_and_carries_non_say_units() {
        let ir = json!({ "units": [
            { "t": "say", "text": "a", "receipt": { "sec": "" } },
            { "t": "pause", "ms": 100 },
            { "t": "say", "text": "b", "receipt": { "sec": "Intro" } },
            { "t": "say", "text": "c", "receipt": { "sec": "Intro" } },
            { "t": "say", "text": "d", "receipt": { "sec": "Body" } },
        ]});
        let secs = sectionize(&ir);
        let arr = secs.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["title"], json!("opening"));
        assert_eq!(arr[0]["from"], json!(0));
        assert_eq!(arr[0]["to"], json!(1));
        assert_eq!(arr[1]["title"], json!("Intro"));
        assert_eq!(arr[1]["from"], json!(2));
        assert_eq!(arr[1]["to"], json!(3));
        assert_eq!(arr[2]["title"], json!("Body"));
        assert_eq!(arr[2]["from"], json!(4));
        assert_eq!(arr[2]["to"], json!(4));
    }

    #[test]
    fn sectionize_empty_units_yields_no_sections() {
        assert_eq!(sectionize(&json!({})), json!([]));
        assert_eq!(sectionize(&json!({ "units": [] })), json!([]));
    }

    #[test]
    fn render_narration_not_found() {
        let c = fresh();
        let err = narrationRender(&json!({ "op": "render", "narration_id": 99 }), &c).unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn render_bad_op_rejected() {
        let c = fresh();
        let id = insert_narration(&c, &ir_no_say(&["a"]));
        let err = narrationRender(&json!({ "op": "nope", "narration_id": id }), &c).unwrap_err();
        assert!(err.contains("op 'render'"), "{err}");
    }

    #[test]
    fn press_with_no_say_units_never_touches_the_deferred_dep() {
        let c = fresh();
        let id = insert_narration(&c, &ir_no_say(&["a"]));
        let r = narrationRender(&json!({ "op": "render", "narration_id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["backend"], json!("kokoro"));
        assert_eq!(r["voice"], json!(DEFAULT_VOICE));
        // pause(200ms) + breath(short procedural) + event(code_omitted, silent) → some non-zero audio
        assert!(r["duration_ms"].as_i64().unwrap() > 0);
        let requested = r["features_requested"].as_array().unwrap();
        assert!(requested.iter().any(|v| v == "pause"));
        assert!(requested.iter().any(|v| v == "breath:short"));
        assert!(requested.iter().any(|v| v == "event:code_omitted"));
        let honored = r["features_honored"].as_array().unwrap();
        assert!(honored.iter().any(|v| v == "event:code_omitted:silent"));
        assert!(r["degraded"].as_array().unwrap().is_empty());

        let count: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.narration_renders", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        let render_id = r["render_id"].as_i64().unwrap();
        let get = narrationRender(&json!({ "op": "render_get", "render_id": render_id }), &c).unwrap();
        assert_eq!(get["ok"], json!(true));
        let abs_path = get["abs_path"].as_str().unwrap().to_string();
        assert!(std::path::Path::new(&abs_path).exists());
        let _ = std::fs::remove_file(&abs_path);
    }

    #[test]
    fn press_with_say_unit_propagates_the_deferred_dependency_error() {
        // What this proves is that a FAILED press leaves no partial db row, so the press must fail
        // for a reason the test owns. kokoroPcm synthesises for real now, so an unguarded run here
        // spawns node and downloads ~90MB, then passes or fails on the network — it failed exactly
        // that way in the full suite while passing alone. Aim the bridge at nothing.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("ANTILOKI_KOKORO_BRIDGE").ok();
        std::env::set_var("ANTILOKI_KOKORO_BRIDGE", "/nonexistent/kokoro-bridge-for-tests.mjs");
        let c = fresh();
        let ir = json!({ "units": [ { "t": "say", "text": "hello", "receipt": { "sec": "" } } ] });
        let id = insert_narration(&c, &ir);
        let err = narrationRender(&json!({ "op": "render", "narration_id": id }), &c).unwrap_err();
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_KOKORO_BRIDGE", v),
            None => std::env::remove_var("ANTILOKI_KOKORO_BRIDGE"),
        }
        assert!(err.contains("kokoroPcm") || err.contains("PARITY-DEFERRED") || !err.is_empty());
        let count: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.narration_renders", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0, "a failed press must not leave a partial db row");
    }

    #[test]
    fn render_get_missing_or_expired_rejected() {
        let c = fresh();
        let err = narrationRender(&json!({ "op": "render_get", "render_id": 999 }), &c).unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn sections_op_reports_units_says_and_est_ms() {
        let c = fresh();
        let ir = json!({ "units": [
            { "t": "say", "text": "one two three", "receipt": { "sec": "" } },
            { "t": "pause", "ms": 100 },
        ]});
        let id = insert_narration(&c, &ir);
        let r = narrationRender(&json!({ "op": "sections", "narration_id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let secs = r["sections"].as_array().unwrap();
        assert_eq!(secs.len(), 1);
        assert_eq!(secs[0]["units"], json!(2));
        assert_eq!(secs[0]["says"], json!(1));
        assert!(secs[0]["est_ms"].as_i64().unwrap() > 100);
        assert_eq!(secs[0]["render_id"], json!(Value::Null));
    }

    #[test]
    fn too_many_says_rejected() {
        let c = fresh();
        let mut units = Vec::new();
        for i in 0..(MAX_SAYS + 1) {
            units.push(json!({ "t": "say", "text": format!("s{i}"), "receipt": { "sec": "" } }));
        }
        let id = insert_narration(&c, &json!({ "units": units }));
        let err = narrationRender(&json!({ "op": "render", "narration_id": id }), &c).unwrap_err();
        assert!(err.contains("press it per section instead"), "{err}");
    }

    #[test]
    fn unknown_section_ord_not_found() {
        let c = fresh();
        let id = insert_narration(&c, &ir_no_say(&["a"]));
        let err = narrationRender(&json!({ "op": "render", "narration_id": id, "section_ord": 7 }), &c).unwrap_err();
        assert!(err.contains("section") && err.contains("not found"), "{err}");
    }

    #[test]
    fn finalize_reports_missing_sections_before_any_are_pressed() {
        let c = fresh();
        let ir = json!({ "units": [
            { "t": "say", "text": "a", "receipt": { "sec": "" } },
            { "t": "say", "text": "b", "receipt": { "sec": "Two" } },
        ]});
        let id = insert_narration(&c, &ir);
        let err = narrationRender(&json!({ "op": "finalize", "narration_id": id }), &c).unwrap_err();
        assert!(err.contains("missing ords"), "{err}");
    }

    #[test]
    fn finalize_trivially_succeeds_when_sectionize_yields_zero_sections() {
        // sectionize() only ever creates a section entry when it walks a `say` unit (see the
        // sectionize tests above) — a narration built from pause/breath/event units alone
        // sectionizes to an EMPTY array. finalize loops that (empty) array, so `missing` stays
        // empty and it concatenates zero parts into an empty (but valid) wav — this is real JS
        // behavior, not a Rust-only special case.
        let c = fresh();
        let id = insert_narration(&c, &ir_no_say(&["a"]));
        let fin = narrationRender(&json!({ "op": "finalize", "narration_id": id }), &c).unwrap();
        assert_eq!(fin["ok"], json!(true));
        assert_eq!(fin["sections"], json!(0));
        assert_eq!(fin["duration_ms"], json!(0));
        let abs_path = fin["abs_path"].as_str().unwrap().to_string();
        assert!(std::path::Path::new(&abs_path).exists());
        let sidecar = std::path::Path::new(&abs_path).with_extension("json");
        assert!(sidecar.exists());
        let _ = std::fs::remove_file(&abs_path);
        let _ = std::fs::remove_file(&sidecar);
    }

    #[test]
    fn progress_op_reports_none_when_nothing_in_flight() {
        let c = fresh();
        let id = insert_narration(&c, &ir_no_say(&["a"]));
        let r = narrationRender(&json!({ "op": "progress", "narration_id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["pressing"], json!(false));
        assert_eq!(r["last_render_id"], json!(Value::Null));
    }

    #[test]
    fn press_progress_lives_in_the_db_so_a_reader_outside_the_pressing_thread_can_see_it() {
        // THE POINT OF THE MOVE. The map this replaced was in the addon's statics, so a press on a
        // worker would be invisible to op:"progress" on the request path. Here the write and the
        // read go through the db, which is what a second thread/connection can also reach.
        let c = fresh();
        let id = insert_narration(&c, &ir_no_say(&["a"]));
        let key = format!("{id}:0");

        progress_begin(&c, &key, id, Some(0), 9).unwrap();
        let r = narrationRender(&json!({ "op": "progress", "narration_id": id, "section_ord": 0 }), &c).unwrap();
        assert_eq!(r["pressing"], json!(true), "a registered press with no render and no error is in flight");
        assert_eq!(r["total"], json!(9));
        assert_eq!(r["done"], json!(0));

        progress_tick(&c, &key, 4, 9);
        let r = narrationRender(&json!({ "op": "progress", "narration_id": id, "section_ord": 0 }), &c).unwrap();
        assert_eq!(r["done"], json!(4), "the bar advances mid-press");
        assert_eq!(r["pressing"], json!(true));

        // render_id APPEARING is what ends the poll — the FE breaks its loop on exactly this.
        progress_done(&c, &key, 9, json!(77));
        let r = narrationRender(&json!({ "op": "progress", "narration_id": id, "section_ord": 0 }), &c).unwrap();
        assert_eq!(r["render_id"], json!(77));
        assert_eq!(r["pressing"], json!(false), "a landed render is no longer pressing");

        // A failure must also stop the poll, or the FE spins forever on a press that is never coming.
        let key2 = format!("{id}:1");
        progress_begin(&c, &key2, id, Some(1), 3).unwrap();
        progress_error(&c, &key2, "kokoro-speak: bridge is missing");
        let r = narrationRender(&json!({ "op": "progress", "narration_id": id, "section_ord": 1 }), &c).unwrap();
        assert_eq!(r["error"], json!("kokoro-speak: bridge is missing"));
        assert_eq!(r["pressing"], json!(false));
    }

    #[test]
    fn background_true_accepts_the_job_and_presses_nothing_until_press_run() {
        // THE INVARIANT. `background: true` must mean "accepted for background execution", never
        // "executed synchronously, answered in a progress-shaped envelope". The second reading is
        // the bug that made a 30s client budget report "could not start" for audio being made, and
        // that froze every other request behind a 50s press on the single-threaded engine.
        let c = fresh();
        let id = insert_narration(&c, &ir_no_say(&["a"]));

        let r = narrationRender(&json!({ "op": "render", "narration_id": id, "background": true }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["started"], json!(true));
        // ACCEPTED ONLY: no receipt row exists yet, because nothing has been pressed.
        let renders = crate::db_json::query_rows(&c, "SELECT id FROM knowledge.narration_renders", &[]).unwrap();
        assert_eq!(renders.as_array().unwrap().len(), 0, "background:true must not press inline");
        // …but the job is registered, so a poll landing right now sees a press in flight rather
        // than an empty slot (which it would resolve to the section's PREVIOUS render).
        let p = narrationRender(&json!({ "op": "progress", "narration_id": id }), &c).unwrap();
        assert_eq!(p["pressing"], json!(true));
        assert_eq!(p["done"], json!(0));

        // The execution half — what the writer lane runs off the request thread.
        let e = narrationRender(&json!({ "op": "press_run", "narration_id": id }), &c).unwrap();
        assert_eq!(e["ok"], json!(true));
        assert!(!e["render_id"].is_null(), "press_run lands the receipt");
        let renders = crate::db_json::query_rows(&c, "SELECT id FROM knowledge.narration_renders", &[]).unwrap();
        assert_eq!(renders.as_array().unwrap().len(), 1, "press_run is what actually presses");

        let p = narrationRender(&json!({ "op": "progress", "narration_id": id }), &c).unwrap();
        assert_eq!(p["pressing"], json!(false), "the poll ends when the render lands");
        assert_eq!(p["render_id"], e["render_id"]);
    }

    #[test]
    fn re_pressing_a_section_replaces_the_previous_attempt_rather_than_inheriting_it() {
        // Stale-job contamination: without INSERT OR REPLACE a finished (or failed) attempt would
        // still be sitting under the same key, and the next press would report the OLD render_id
        // as its own — a progress bar that is instantly, wrongly complete.
        let c = fresh();
        let id = insert_narration(&c, &ir_no_say(&["a"]));
        let key = format!("{id}:0");
        progress_begin(&c, &key, id, Some(0), 4).unwrap();
        progress_done(&c, &key, 4, json!(11));

        progress_begin(&c, &key, id, Some(0), 6).unwrap();
        let r = narrationRender(&json!({ "op": "progress", "narration_id": id, "section_ord": 0 }), &c).unwrap();
        assert_eq!(r["render_id"], json!(Value::Null), "the previous attempt's receipt must not carry over");
        assert_eq!(r["total"], json!(6));
        assert_eq!(r["done"], json!(0));
        assert_eq!(r["pressing"], json!(true));
    }
}
