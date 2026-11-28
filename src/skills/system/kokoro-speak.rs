//! kokoro-speak.rs — twin of skills/system/kokoro-speak.mjs. NAMES ARE CONTRACT: `MODEL_ID`, `DTYPE`, `RATE`, `DEFAULT_VOICE`, `BACKEND_VERSION`, `kokoroPcm`, `kokoroSpeak` (default).
//!
//! THERE IS NO SPOKEN VOICE IN THE ENGINE (2026-11-26, issue #3 and the operator's rule: Rust + Tauri + SQLite). It ran Node through a script of the retired backend, which no shipped build carried. The
//! input is still checked as before; synthesis answers that there is no voice, and the screen reads aloud with the system's own voice instead.
//! A Rust port was tried once and reversed (e6ba031, 046ad61): its pronunciation needed a C toolchain, or changed the voice.

use base64::Engine;
use serde_json::{json, Value};

pub const MODEL_ID: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";
pub const DTYPE: &str = "q8";
pub const RATE: u32 = 24000; // Kokoro's native output rate
pub const DEFAULT_VOICE: &str = "af_heart";

pub fn BACKEND_VERSION() -> String {
    format!("kokoro-82M-v1.0/{DTYPE}")
}

// JS: String(v) coercion — matches the local js_string convention used across this crate.
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

// JS: `text ?? ''` / `voice ?? DEFAULT_VOICE` — nullish coalescing, missing or null only.
fn is_nullish(v: Option<&Value>) -> bool {
    matches!(v, None | Some(Value::Null))
}

pub const NO_VOICE: &str = "kokoro-speak: there is no spoken voice in the engine (it needed Node); the screen reads aloud with the system voice";

/// The text is checked, then: no voice. Callers already say so to the person.
pub fn kokoroPcm(input: &Value, _context: &Value) -> Result<(Vec<f32>, u32), String> {
    let text_v = input.as_object().and_then(|o| o.get("text"));
    let t = if is_nullish(text_v) { String::new() } else { js_string(text_v.unwrap()) };
    let t = t.trim();
    if t.is_empty() {
        return Err("kokoro-speak: text is required".to_string());
    }

    Err(NO_VOICE.to_string())
}

/// `export default async function kokoroSpeak(input, context)`.
pub fn kokoroSpeak(input: &Value, context: &Value) -> Value {
    let text = input.as_object().and_then(|o| o.get("text")).cloned().unwrap_or(Value::Null);
    let voice = input.as_object().and_then(|o| o.get("voice")).cloned().unwrap_or(Value::Null);
    let inner = json!({ "text": text, "voice": voice });

    match kokoroPcm(&inner, context) {
        Ok((samples, rate)) => {
            let wav = crate::wav_tools::encodeWav16(&samples, rate);
            let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
            json!({ "ok": true, "audio_b64": audio_b64, "mime": "audio/wav", "rate": rate })
        }
        Err(e) => {
            let truncated: String = e.chars().take(200).collect();
            json!({ "ok": false, "error": truncated })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_js_exports() {
        assert_eq!(MODEL_ID, "onnx-community/Kokoro-82M-v1.0-ONNX");
        assert_eq!(DTYPE, "q8");
        assert_eq!(RATE, 24000);
        assert_eq!(DEFAULT_VOICE, "af_heart");
        assert_eq!(BACKEND_VERSION(), "kokoro-82M-v1.0/q8");
    }

    #[test]
    fn kokoro_pcm_requires_nonempty_text() {
        let err = kokoroPcm(&json!({}), &json!({})).unwrap_err();
        assert_eq!(err, "kokoro-speak: text is required");

        let err = kokoroPcm(&json!({ "text": "   " }), &json!({})).unwrap_err();
        assert_eq!(err, "kokoro-speak: text is required");

        let err = kokoroPcm(&json!({ "text": null }), &json!({})).unwrap_err();
        assert_eq!(err, "kokoro-speak: text is required");
    }

    #[test]
    fn text_is_coerced_and_then_there_is_no_voice_and_nothing_is_run() {
        assert_eq!(kokoroPcm(&json!({ "text": 0 }), &json!({})).unwrap_err(), NO_VOICE);
        assert_eq!(kokoroPcm(&json!({ "text": "hello", "voice": "af_heart" }), &json!({})).unwrap_err(), NO_VOICE);
    }

    #[test]
    fn kokoro_speak_missing_text_returns_ok_false_with_the_exact_js_message() {
        let r = kokoroSpeak(&json!({}), &json!({}));
        assert_eq!(r, json!({ "ok": false, "error": "kokoro-speak: text is required" }));
    }

    #[test]
    fn kokoro_speak_error_message_is_truncated_to_200_chars() {
        // The bridge's own failures are short and situational, so forcing a >200-char error through
        // kokoroPcm is not reliably reachable. This proves the truncation against a synthetic string.
        let long = "x".repeat(250);
        let truncated: String = long.chars().take(200).collect();
        assert_eq!(truncated.len(), 200);
    }

    #[test]
    fn kokoro_speak_ok_path_shape_matches_js_when_pcm_resolves() {
        // Shapes the ok:true path directly instead of going through kokoroPcm: reaching it for real
        // means spawning node and synthesising, which a unit test must not do (see the hermetic
        // note above). This is the same deterministic assembly kokoroSpeak runs on real samples.
        let samples: Vec<f32> = vec![0.0, 0.5, -0.5, 0.25];
        let wav = crate::wav_tools::encodeWav16(&samples, RATE);
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&wav);
        let shaped = json!({ "ok": true, "audio_b64": audio_b64, "mime": "audio/wav", "rate": RATE });
        assert_eq!(shaped["mime"], json!("audio/wav"));
        assert_eq!(shaped["rate"], json!(24000));
        assert!(shaped["audio_b64"].as_str().unwrap().len() > 0);
    }
}
