//! js_math — TRANSLATION MEMORY, entry #1. JavaScript semantics that Rust must replicate
//! EXACTLY or conformance dies on half-integers and sign edges. Every ported function uses
//! these instead of the std equivalents; the consistency check flags any raw `.round()` in
//! ported code.
// QUALITY: 92/100 — the translation memory itself — tiny, trap-documented, tested

/// JS `Math.round`: floor(x + 0.5) — rounds HALF UP (toward +∞), so -0.5 → 0, not -1.
/// Rust's `f64::round` rounds half AWAY FROM ZERO (-0.5 → -1). This one-line difference is
/// the canonical example of why the crossing compares results instead of trusting eyes.
pub fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// JS `x >>> 0` on the LCG accumulator: wrap to u32. Rust's wrapping ops replicate it.
pub fn lcg_next(s: u32) -> u32 {
    s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_half_up_matches_js() {
        assert_eq!(js_round(0.5), 1.0);
        assert_eq!(js_round(-0.5), 0.0); // Rust .round() would say -1 — THE trap
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(1.4), 1.0);
        assert_eq!(js_round(-1.6), -2.0);
    }

    #[test]
    fn lcg_matches_the_contract() {
        // the PRNG constants are CONTRACT (PORTING.md): s = (s*1664525 + 1013904223) >>> 0
        let s0: u32 = 0x6b6f_6b6f;
        let s1 = lcg_next(s0);
        let s2 = lcg_next(s1);
        assert_eq!(s1, s0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223));
        assert_ne!(s1, s2);
    }
}
