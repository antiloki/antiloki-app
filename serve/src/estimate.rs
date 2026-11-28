//! WHAT IT WOULD HAVE COST, WHEN NOBODY SAID (operator 2026-09-26: "when we dont have natively that report we
//! should have a deterministic skill calculator based on the real oficial token expense from than and counting
//! tokens sent and received. So we dont really need their report to estimate").
//!
//! `cli::prices_of` already knows what a model charges per million tokens. `cli::price_tokens` already turns
//! counts into dollars. What was missing is the counts: an engine on a subscription often reports no usage at
//! all, and a missing count priced as zero is the most confident lie the product can tell — it makes the
//! cheapest thing on the screen the one nobody measured.
//!
//! THE POINT IS NOT PRECISION, IT IS A NUMBER WITH A KNOWN NAME. An estimate and a measurement are different
//! facts, and `Basis` keeps them apart everywhere they are shown. A number the operator cannot tell apart from a
//! measurement is worse than no number, which is why `Unknown` stays a real answer rather than a zero.
use serde::{Deserialize, Serialize};

/// WHERE A PRICE CAME FROM. Shown wherever the price is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Basis {
    /// the engine told us what it spent
    Reported,
    /// we counted the tokens and multiplied by the published price
    Estimated,
    /// nobody said and there was nothing to count — NOT zero
    Unknown,
}

impl Basis {
    /// the word the screen prints beside the number. `Estimated` carries the `≈` so a caller cannot forget it.
    pub fn word(self) -> &'static str {
        match self { Basis::Reported => "measured", Basis::Estimated => "≈ estimated", Basis::Unknown => "not priced" }
    }
    pub fn key(self) -> &'static str {
        match self { Basis::Reported => "reported", Basis::Estimated => "estimated", Basis::Unknown => "unknown" }
    }
}

/// TOKENS IN A TEXT, WITHOUT A TOKENIZER.
///
/// A real BPE tokenizer is a dependency per vendor and a different answer per model, for a number we then
/// multiply by a price that is itself a published round figure. What is needed is one deterministic count with a
/// KNOWN bias, not a perfect one.
///
/// The rule, which is the common approximation for BPE vocabularies: an ASCII run costs about one token per four
/// characters, and a character outside ASCII (CJK, emoji, most accented text) costs about one token on its own —
/// those are the two regimes every byte-pair vocabulary has. Code sits at the dense end of the ASCII regime
/// because of its punctuation, so this UNDER-counts code slightly, which is the safe direction for a number the
/// operator compares against a bill.
///
/// Deterministic, allocation-free, and pure — the same text always answers the same. Tested.
pub fn tokens_of(text: &str) -> i64 {
    let (mut ascii, mut wide) = (0i64, 0i64);
    for ch in text.chars() {
        if ch.is_ascii() { ascii += 1 } else { wide += 1 }
    }
    // a non-empty text is never zero tokens
    let n = (ascii + 3) / 4 + wide;
    if text.is_empty() { 0 } else { n.max(1) }
}

/// The cost of one exchange we had to count ourselves. `None` when the model is not one we have a price for —
/// said as `Unknown`, never as 0.0.
pub fn estimate(engine: crate::cli::Engine, model: Option<&str>, sent: &str, got: &str) -> (f64, Basis) {
    let (tin, tout) = (tokens_of(sent), tokens_of(got));
    if tin == 0 && tout == 0 { return (0.0, Basis::Unknown) }
    (crate::cli::price_tokens(engine, model, tin, tout, 0, 0), Basis::Estimated)
}

/// THE PRICE OF A TURN, PREFERRING WHAT THE ENGINE SAID. The one function a caller should reach for: a reported
/// figure wins, a counted one stands in, and "nobody said and there was nothing to count" is its own answer.
///
/// `reported` is the engine's own number when it gave one. Pure — tested.
pub fn price_turn(reported: Option<f64>, engine: crate::cli::Engine, model: Option<&str>, sent: &str, got: &str) -> (f64, Basis) {
    match reported {
        // A REPORTED ZERO IS STILL A REPORT. An engine that genuinely spent nothing (a cached turn, a refusal
        // before any call) said so, and overriding it with an estimate would invent a cost that was not paid.
        Some(usd) if usd.is_finite() && usd >= 0.0 => (usd, Basis::Reported),
        _ => estimate(engine, model, sent, got),
    }
}

/// THE PRICE OF A RUN WE ACTUALLY MADE. `price_turn` above takes one number and cannot tell "the engine said
/// zero" from "the engine says nothing about money". A `CliTrace` can, because `run_cli_inner` already separated
/// them: `reported` is the engine's own figure and is only ever kept when it is positive, while `cost` is that
/// figure or, failing it, the API-equivalent of the tokens the engine DID report. Prefer this wherever a run is
/// in hand.
///
/// A FLAT SEAT IS NOT A FREE TURN (2026-09-28). Cursor, Antigravity and Grok print no price because none is
/// charged — they are subscriptions. Reading their silent `0` as a report is what wrote `$0.00` into 47 live
/// merge rows and then told the operator four benchmark cells were free to settle. Their tokens are real, so
/// `trace.cost` prices them and this returns `Estimated`.
///
/// A REPLAYED TURN REALLY IS FREE, and keeps its zero: `CliTrace::replayed` carries `cost: Some(0.0)` with zero
/// tokens, so it never reaches the text estimate below.
pub fn price_run(trace: &crate::cli::CliTrace, engine: crate::cli::Engine, model: Option<&str>, sent: &str, got: &str) -> (f64, Basis) {
    if let Some(usd) = trace.reported.filter(|c| c.is_finite() && *c > 0.0) { return (usd, Basis::Reported) }
    if let Some(usd) = trace.cost.filter(|c| c.is_finite() && *c >= 0.0) { return (usd, Basis::Estimated) }
    estimate(engine, model, sent, got)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Engine;

    /// A FLAT SEAT IS NOT A FREE TURN (2026-09-28). Cursor, Antigravity and Grok are subscriptions: they print
    /// tokens but never a price, and the Claude-shaped parser fills the missing figure with `0`. Reading that as a
    /// report is what put `usd_total = 0` on 47 of 186 live merge rows (25 planner, 17 qa-gate, 4 verifier, 1
    /// editor) and then told the operator that four benchmark cells were "$0.00 to settle". `price_run` reads the
    /// trace instead, where `run_cli_inner` has already kept `reported` only when it is positive and priced the
    /// engine's own tokens into `cost`.
    #[test]
    fn a_seat_that_prints_no_price_is_priced_from_the_tokens_it_did_print() {
        let mk = |reported: Option<f64>, cost: Option<f64>| crate::cli::CliTrace { model: None,
            id: 0, engine: Engine::Cursor, cwd: String::new(), started: 0, ended: 1, ms: 1, tools: vec![],
            tokens_in: Some(1000), tokens_out: Some(500), reported, cost, chars: 8, ok: true, error: None,
        };
        let (sent, got) = ("the ask", "the answer");

        // the bug: a silent seat whose tokens were priced anyway — a real number, and NOT called a report
        let t = mk(None, Some(0.0123));
        let (usd, basis) = price_run(&t, Engine::Cursor, Some("composer-2.5"), sent, got);
        assert_eq!((usd, basis.key()), (0.0123, "estimated"), "the tokens are real even when the price is not printed");

        // an engine that really did say a number keeps it, and keeps the stronger word for it
        let t = mk(Some(0.42), Some(0.42));
        assert_eq!(price_run(&t, Engine::ClaudeCode, Some("opus"), sent, got), (0.42, Basis::Reported));

        // A REPLAYED TURN REALLY IS FREE and must not be re-estimated from its text.
        let t = crate::cli::CliTrace::replayed(Engine::ClaudeCode);
        let (usd, basis) = price_run(&t, Engine::ClaudeCode, Some("opus"), sent, "a long plan that would price well above zero if we counted its characters");
        assert_eq!((usd, basis.key()), (0.0, "estimated"), "a cache hit spent nothing and stays at nothing");

        // and nothing to go on at all still falls back to counting the text
        let t = mk(None, None);
        let (usd, basis) = price_run(&t, Engine::ClaudeCode, Some("opus"), "a long ask that costs real money", got);
        assert!(usd > 0.0 && basis == Basis::Estimated, "got ${usd} {}", basis.key());
    }

    #[test]
    fn a_count_is_deterministic_and_never_zero_for_a_non_empty_text() {
        assert_eq!(tokens_of(""), 0, "nothing is nothing");
        assert_eq!(tokens_of("a"), 1, "a non-empty text is never zero tokens");
        assert_eq!(tokens_of("the quick brown fox"), tokens_of("the quick brown fox"), "same text, same answer");
        // the ASCII regime: about four characters to the token
        let prose = "the quick brown fox jumps over the lazy dog";  // 43 chars
        assert_eq!(tokens_of(prose), 11);
        // the wide regime: a character outside ASCII is about a token on its own
        assert_eq!(tokens_of("日本語"), 3);
        // and the two compose rather than one swallowing the other
        assert_eq!(tokens_of("ok 日本語"), (3 + 3) / 4 + 3);
    }

    #[test]
    fn an_estimate_is_priced_at_the_published_rate_and_says_it_is_an_estimate() {
        // 4000 ascii chars in, 400 out → 1000 tokens in, 100 out. Sonnet: $3/M in, $15/M out.
        let sent = "x".repeat(4000);
        let got = "y".repeat(400);
        let (usd, basis) = estimate(Engine::ClaudeCode, Some("sonnet"), &sent, &got);
        assert_eq!(basis, Basis::Estimated);
        let want = (1000.0 * 3.0 + 100.0 * 15.0) / 1e6;
        assert!((usd - want).abs() < 1e-9, "{usd} vs {want}");
        assert!(basis.word().contains('≈'), "an estimate carries its own ≈ so a screen cannot drop it");
    }

    #[test]
    fn nothing_to_count_is_unknown_and_never_zero_dollars() {
        let (usd, basis) = estimate(Engine::ClaudeCode, Some("sonnet"), "", "");
        assert_eq!((usd, basis), (0.0, Basis::Unknown));
        assert_eq!(Basis::Unknown.word(), "not priced", "a screen must not print $0.00 for this");
    }

    #[test]
    fn what_the_engine_said_beats_what_we_counted_including_a_reported_zero() {
        let sent = "x".repeat(4000);
        // a real report wins
        let (usd, basis) = price_turn(Some(0.42), Engine::ClaudeCode, Some("opus"), &sent, "hi");
        assert_eq!((usd, basis), (0.42, Basis::Reported));
        // …and a reported ZERO is a report, not a missing value: an engine that spent nothing said so
        let (usd0, b0) = price_turn(Some(0.0), Engine::ClaudeCode, Some("opus"), &sent, "hi");
        assert_eq!((usd0, b0), (0.0, Basis::Reported));
        // nothing reported → we count, and say we counted
        let (_, b1) = price_turn(None, Engine::ClaudeCode, Some("opus"), &sent, "hi");
        assert_eq!(b1, Basis::Estimated);
        // a NaN is not a report
        let (_, b2) = price_turn(Some(f64::NAN), Engine::ClaudeCode, Some("opus"), &sent, "hi");
        assert_eq!(b2, Basis::Estimated);
    }

    #[test]
    fn a_cheaper_model_estimates_cheaper_on_the_same_text() {
        let sent = "x".repeat(40_000);
        let (opus, _) = estimate(Engine::ClaudeCode, Some("opus"), &sent, "y");
        let (qwen, _) = estimate(Engine::Opencode, Some("opencode/qwen3.6-plus"), &sent, "y");
        assert!(qwen < opus, "the whole product claim: {qwen} must be under {opus}");
    }
}
