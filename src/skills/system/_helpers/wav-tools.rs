//! wav-tools — the Rust twin of skills/system/_helpers/wav-tools.mjs, statement for statement.
//! NAMES ARE CONTRACT: `encodeWav16`, `decodeWav16`, `resampleNearest`, `silence`,
//! `proceduralBreath`, `concatF32`, `normalize` — exactly the JS exports; locals mirror the JS
//! locals. Where JS semantics diverge from Rust defaults (Math.round), js_math carries the bridge.
// QUALITY: 85/100 — faithful port, solid tests; JS-mirroring index loops over idiom — fidelity chosen on purpose

use crate::js_math::{js_round, lcg_next};

/// Float32 samples (-1..1) → 16-bit PCM mono WAV buffer. Mirrors encodeWav16 line for line.
pub fn encodeWav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let n = samples.len();
    let mut buf = Vec::with_capacity(44 + n * 2);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + (n as u32) * 2).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&rate.to_le_bytes());
    buf.extend_from_slice(&(rate * 2).to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&((n as u32) * 2).to_le_bytes());
    for i in 0..n {
        let s = (samples[i] as f64).clamp(-1.0, 1.0); // JS: Math.max(-1, Math.min(1, samples[i]))
        let v = js_round(s * 32767.0) as i16; // JS Math.round — NOT f64::round (see js_math)
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf
}

pub struct DecodedWav {
    pub samples: Vec<f32>,
    pub rate: u32,
}

/// Minimal WAV reader for event clips: PCM16, first channel taken if stereo.
pub fn decodeWav16(buf: &[u8]) -> Option<DecodedWav> {
    if buf.len() < 44 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return None;
    }
    let mut off = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // codec, channels, rate, bits
    let mut data: Option<&[u8]> = None;
    while off + 8 <= buf.len() {
        let id = &buf[off..off + 4];
        let size = u32::from_le_bytes(buf[off + 4..off + 8].try_into().ok()?) as usize;
        if id == b"fmt " && off + 8 + 16 <= buf.len() {
            fmt = Some((
                u16::from_le_bytes(buf[off + 8..off + 10].try_into().ok()?),
                u16::from_le_bytes(buf[off + 10..off + 12].try_into().ok()?),
                u32::from_le_bytes(buf[off + 12..off + 16].try_into().ok()?),
                u16::from_le_bytes(buf[off + 22..off + 24].try_into().ok()?),
            ));
        }
        if id == b"data" {
            let end = (off + 8 + size).min(buf.len());
            data = Some(&buf[off + 8..end]);
            break;
        }
        off += 8 + size + (size % 2);
    }
    let (codec, channels, rate, bits) = fmt?;
    let data = data?;
    if codec != 1 || bits != 16 {
        return None;
    }
    let frames = data.len() / 2 / channels as usize;
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let idx = i * 2 * channels as usize;
        out.push(i16::from_le_bytes([data[idx], data[idx + 1]]) as f32 / 32768.0);
    }
    Some(DecodedWav { samples: out, rate })
}

/// Nearest-neighbor resample — good enough for breaths and room tone, not for speech.
pub fn resampleNearest(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return samples.to_vec();
    }
    let n = js_round(samples.len() as f64 * to as f64 / from as f64) as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // JS: samples[Math.min(samples.length - 1, Math.round((i * from) / to))]
        let src = js_round(i as f64 * from as f64 / to as f64) as usize;
        out.push(samples[src.min(samples.len().saturating_sub(1))]);
    }
    out
}

pub fn silence(ms: f64, rate: u32) -> Vec<f32> {
    let n = js_round(ms / 1000.0 * rate as f64).max(0.0) as usize; // JS: Math.max(0, Math.round(...))
    vec![0.0; n]
}

/// Procedural breath v2 — quiet intake: seeded LCG (constants are CONTRACT), moving-average
/// band, sine^1.8 envelope, gain 0.16. Deterministic: the same breath every render, both languages.
pub fn proceduralBreath(kind: &str, rate: u32) -> Vec<f32> {
    let ms = if kind == "long" { 480.0 } else { 260.0 };
    let n = js_round(ms / 1000.0 * rate as f64) as usize;
    let mut seed: u32 = 0x6b6f_6b6f; // fixed — the same breath every render, determinism over variety
    let mut rand = || {
        seed = lcg_next(seed);
        seed as f64 / 4294967296.0 - 0.5
    };
    let raw: Vec<f64> = (0..n).map(|_| rand()).collect();
    let ma = |src: &[f64], w: usize| -> Vec<f64> {
        let mut out = vec![0.0; src.len()];
        let mut acc = 0.0;
        for i in 0..src.len() {
            acc += src[i];
            if i >= w {
                acc -= src[i - w];
            }
            out[i] = acc / w as f64;
        }
        out
    };
    let fast = ma(&raw, 8);
    let slow = ma(&raw, 40);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let x = i as f64 / n as f64;
        let env = (std::f64::consts::PI * x).sin().powf(1.8); // smooth in AND out — an intake, not a burst
        out.push(((fast[i] - slow[i]) * env * 0.16) as f32);
    }
    out
}

/// Concatenate Float32 chunks.
pub fn concatF32(chunks: &[Vec<f32>]) -> Vec<f32> {
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    let mut out = Vec::with_capacity(total);
    for c in chunks {
        out.extend_from_slice(c);
    }
    out
}

/// Peak-normalize to a printed ceiling so stitched segments share loudness headroom.
pub fn normalize(samples: &[f32], ceiling: f64) -> Vec<f32> {
    let mut peak = 0.0f64;
    for i in 0..samples.len() {
        peak = peak.max((samples[i] as f64).abs());
    }
    if peak < 1e-6 || peak <= ceiling {
        return samples.to_vec();
    }
    let g = ceiling / peak;
    samples.iter().map(|&s| ((s as f64) * g) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_roundtrip_16bit_faithful() {
        let s = [0.0f32, 0.5, -0.5, 0.25];
        let bytes = encodeWav16(&s, 24000);
        let back = decodeWav16(&bytes).expect("decodes");
        assert_eq!(back.rate, 24000);
        for (a, b) in s.iter().zip(back.samples.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn breath_shape_and_determinism() {
        let a = proceduralBreath("short", 24000);
        let b = proceduralBreath("short", 24000);
        assert_eq!(a.len(), 6240); // 260ms at 24k
        assert_eq!(a, b, "the same breath every render — determinism over variety");
        assert!(a.iter().all(|v| v.abs() < 0.5), "quiet by design (gain .16)");
    }

    #[test]
    fn silence_is_exact_zeros() {
        let z = silence(120.0, 24000);
        assert_eq!(z.len(), 2880);
        assert!(z.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn normalize_leaves_headroom_alone() {
        let quiet = [0.1f32, -0.2];
        assert_eq!(normalize(&quiet, 0.9), quiet.to_vec());
        let loud = normalize(&[0.0f32, 2.0, -4.0], 0.9);
        assert!((loud[2].abs() - 0.9).abs() < 1e-6, "peak lands on the ceiling");
    }
}
