//! THE MICROPHONE (operator 2026-11-11: "a microphone icon on the agent: on click we talk, it is transcribed and sent to the agent, like a local whisper").
//!
//! The browser records, this module transcribes ON THIS MACHINE and hands the words back; nothing is sent anywhere. The audio (whatever the browser's recorder made: webm/opus, mp4, wav) goes through `ffmpeg` to 16 kHz mono wav, then to the first
//! speech-to-text this machine has, in this order:
//!   1. `ANTILOKI_WHISPER_CMD`  any command that takes the wav path as its last argument and prints the words (the person's own setup, and how the journeys stand in a model)
//!   2. whisper.cpp             `whisper-cli` / `whisper-cpp` on the PATH, with `ANTILOKI_WHISPER_MODEL` naming a ggml model file
//!   3. faster-whisper          through `uv run --script` (its own cached environment; the model is `ANTILOKI_WHISPER_MODEL_NAME`, default `small`, language `ANTILOKI_WHISPER_LANG`, or detected among `ANTILOKI_WHISPER_LANGS` (default en,pt); it transcribes, it never translates)
//! Nothing is installed by itself: with none of them the answer says what to install.
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 25 * 1024 * 1024;
const SCRIPT: &str = r#"# /// script
# requires-python = ">=3.9"
# dependencies = ["faster-whisper"]
# ///
# The wav is read here (16 kHz mono, made by ffmpeg) and handed over as samples: faster-whisper's own file reader (PyAV) breaks on some version pairs.
import os, sys, wave
import numpy as np
from faster_whisper import WhisperModel
m = WhisperModel(os.environ.get("ANTILOKI_WHISPER_MODEL_NAME", "small"), device="cpu", compute_type="int8")
w = wave.open(sys.argv[1]); audio = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32) / 32768.0
# Keep the words as they are said, and do not let a short sentence be mistaken for another language: the language is chosen among the ones this person speaks (ANTILOKI_WHISPER_LANGS, default en,pt) by the model's own probabilities, unless ANTILOKI_WHISPER_LANG fixes it.
cands = [c.strip() for c in os.environ.get("ANTILOKI_WHISPER_LANGS", "en,pt").split(",") if c.strip()]
def pick(a):
    try:
        r = m.detect_language(a); probs = dict(r[2]) if len(r) > 2 and r[2] else {}
        return max((c for c in cands if c in probs), key=lambda c: probs[c], default=None) or r[0]
    except Exception:
        return None
lang = os.environ.get("ANTILOKI_WHISPER_LANG") or pick(audio)
segs, _ = m.transcribe(audio, language=lang, task="transcribe", beam_size=1, vad_filter=True, condition_on_previous_text=False)
print(" ".join(s.text.strip() for s in segs))
"#;

#[derive(Debug, Clone, PartialEq)]
pub enum Engine { Custom(Vec<String>), WhisperCpp { bin: String, model: String }, FasterWhisper }
impl Engine { pub fn name(&self) -> &'static str { match self { Engine::Custom(_) => "your command", Engine::WhisperCpp { .. } => "whisper.cpp", Engine::FasterWhisper => "faster-whisper" } } }

/// Which speech-to-text to use, from what the environment and the PATH offer. `has(bin)` says whether a program is on the PATH. Pure over its inputs - tested.
pub fn choose(env_cmd: Option<&str>, env_model: Option<&str>, has: &dyn Fn(&str) -> bool) -> Option<Engine> {
    if let Some(c) = env_cmd.map(str::trim).filter(|c| !c.is_empty()) { return Some(Engine::Custom(c.split_whitespace().map(String::from).collect())); }
    if let Some(m) = env_model.map(str::trim).filter(|m| !m.is_empty() && Path::new(m).is_file()) { for bin in ["whisper-cli", "whisper-cpp"] { if has(bin) { return Some(Engine::WhisperCpp { bin: bin.into(), model: m.into() }); } } }
    if has("uv") { return Some(Engine::FasterWhisper); }
    None
}
fn has_on_path(bin: &str) -> bool { std::env::var_os("PATH").map_or(false, |p| std::env::split_paths(&p).any(|d| d.join(bin).is_file())) }
pub fn engine() -> Option<Engine> { choose(std::env::var("ANTILOKI_WHISPER_CMD").ok().as_deref(), std::env::var("ANTILOKI_WHISPER_MODEL").ok().as_deref(), &has_on_path) }

/// What a transcript is worth keeping: whitespace folded; a result of one or two characters, or a bare caption such as `[BLANK_AUDIO]`, is silence. Pure.
pub fn clean_transcript(raw: &str) -> String {
    let t: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let bare = t.starts_with('[') && t.ends_with(']') || t.starts_with('(') && t.ends_with(')') && t.len() < 40;
    if bare || t.chars().count() < 2 { String::new() } else { t }
}

fn reply(code: StatusCode, v: Value) -> Response { (code, Json(v)).into_response() }
fn script_path() -> PathBuf { crate::remote::dir().join("voice").join("transcribe.py") }

/// `GET /api/v1/voice/status`: is there a way to turn speech into words here, and what it is.
pub async fn status() -> Response {
    let e = engine(); let ffmpeg = has_on_path("ffmpeg");
    reply(StatusCode::OK, json!({ "ok": true, "available": e.is_some() && ffmpeg, "engine": e.as_ref().map(|e| e.name()), "ffmpeg": ffmpeg,
        "hint": if !ffmpeg { json!("install ffmpeg: it turns the recording into something a speech model reads") } else if e.is_none() { json!("install uv (https://docs.astral.sh/uv/) and antiloki uses faster-whisper, or set ANTILOKI_WHISPER_CMD to your own transcriber") } else { Value::Null } }))
}

async fn run(bin: &str, args: &[String], env: &[(&str, String)]) -> Result<String, String> {
    let mut c = crate::proc::tokio_cmd(bin); c.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    for (k, v) in env { c.env(k, v); }
    let out = tokio::time::timeout(std::time::Duration::from_secs(180), c.output()).await.map_err(|_| format!("{bin} took too long"))?.map_err(|e| format!("could not run {bin}: {e}"))?;
    if out.status.success() { Ok(String::from_utf8_lossy(&out.stdout).to_string()) } else { Err(format!("{bin} failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").chars().take(200).collect::<String>())) }
}

/// `POST /api/v1/voice/transcribe` with the recording as the body (`?lang=` optional): the words, from a model on this machine.
pub async fn transcribe(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>, body: Bytes) -> Response {
    if body.is_empty() { return reply(StatusCode::OK, json!({ "ok": false, "error": "no audio" })); }
    if body.len() > MAX_BYTES { return reply(StatusCode::PAYLOAD_TOO_LARGE, json!({ "ok": false, "error": "the recording is too long" })); }
    let Some(e) = engine() else { return reply(StatusCode::OK, json!({ "ok": false, "error": "no speech-to-text here: install uv (antiloki then uses faster-whisper), or set ANTILOKI_WHISPER_CMD" })); };
    if !has_on_path("ffmpeg") { return reply(StatusCode::OK, json!({ "ok": false, "error": "ffmpeg is not installed: it is needed to read the recording" })); }
    let dir = std::env::temp_dir().join(format!("antiloki-voice-{}-{}", std::process::id(), crate::cli::now_ms())); if std::fs::create_dir_all(&dir).is_err() { return reply(StatusCode::OK, json!({ "ok": false, "error": "could not make a place for the recording" })); }
    let (inp, wav) = (dir.join("in.bin"), dir.join("out.wav"));
    let result: Result<String, String> = async {
        std::fs::write(&inp, &body).map_err(|e| e.to_string())?;
        run("ffmpeg", &["-y".into(), "-loglevel".into(), "error".into(), "-i".into(), inp.to_string_lossy().into(), "-ac".into(), "1".into(), "-ar".into(), "16000".into(), wav.to_string_lossy().into()], &[]).await.map_err(|e| format!("could not read the recording ({e})"))?;
        let w = wav.to_string_lossy().to_string();
        let lang = q.get("lang").map(|l| l.chars().filter(|c| c.is_ascii_alphabetic()).take(5).collect::<String>()).unwrap_or_default();
        match &e {
            Engine::Custom(cmd) => { let mut a: Vec<String> = cmd[1..].to_vec(); a.push(w); run(&cmd[0], &a, &[("ANTILOKI_WHISPER_LANG", lang)]).await }
            Engine::WhisperCpp { bin, model } => { let mut a: Vec<String> = vec!["-m".into(), model.clone(), "-f".into(), w, "-nt".into(), "-np".into()]; if !lang.is_empty() { a.extend(["-l".into(), lang]); } run(bin, &a, &[]).await }
            Engine::FasterWhisper => {
                let sp = script_path(); if let Some(d) = sp.parent() { let _ = std::fs::create_dir_all(d); } if std::fs::read_to_string(&sp).ok().as_deref() != Some(SCRIPT) { std::fs::write(&sp, SCRIPT).map_err(|e| e.to_string())?; }
                run("uv", &["run".into(), "--quiet".into(), "--script".into(), sp.to_string_lossy().into(), w], &[("ANTILOKI_WHISPER_LANG", lang)]).await
            }
        }
    }.await;
    let _ = std::fs::remove_dir_all(&dir);
    match result { Ok(t) => reply(StatusCode::OK, json!({ "ok": true, "text": clean_transcript(&t), "engine": e.name() })), Err(er) => reply(StatusCode::OK, json!({ "ok": false, "error": er })) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_first_speech_to_text_this_machine_has_is_the_one_used() {
        let none = |_: &str| false; let all = |_: &str| true; let only = |b: &str| b == "uv";
        assert_eq!(choose(Some("my-stt --fast"), None, &all), Some(Engine::Custom(vec!["my-stt".into(), "--fast".into()])), "the person's own command wins");
        let model = std::env::temp_dir().join(format!("antiloki-ggml-{}.bin", std::process::id())); std::fs::write(&model, "x").unwrap();
        assert_eq!(choose(None, Some(model.to_str().unwrap()), &all), Some(Engine::WhisperCpp { bin: "whisper-cli".into(), model: model.to_string_lossy().into() }));
        assert_eq!(choose(None, Some(model.to_str().unwrap()), &only), Some(Engine::FasterWhisper), "no whisper.cpp binary: on to uv"); assert_eq!(choose(None, Some("/no/such/model.bin"), &only), Some(Engine::FasterWhisper), "a model file that is not there is not a model");
        assert_eq!(choose(None, None, &only), Some(Engine::FasterWhisper)); assert_eq!(choose(None, None, &none), None); assert_eq!(choose(Some("  "), None, &none), None);
        let _ = std::fs::remove_file(model);
    }
    #[test]
    fn silence_and_captions_are_not_words() {
        assert_eq!(clean_transcript("  open the   cart\n file "), "open the cart file"); assert_eq!(clean_transcript(" [BLANK_AUDIO] "), ""); assert_eq!(clean_transcript("(silence)"), ""); assert_eq!(clean_transcript("a"), ""); assert_eq!(clean_transcript(""), "");
        assert_eq!(clean_transcript("abre o carrinho (de compras)"), "abre o carrinho (de compras)");
    }
    #[test] fn the_script_transcribes_and_never_translates_and_picks_among_the_persons_languages() {
        assert!(SCRIPT.contains("task=\"transcribe\"") && !SCRIPT.contains("translate")); assert!(SCRIPT.contains("ANTILOKI_WHISPER_LANGS") && SCRIPT.contains("detect_language"), "the language is chosen among the candidates, not by the model's first guess over every language");
    }
}
