//! failure-classes.rs — twin of skills/ai/failure-classes.mjs (pure, no-db). NAMES ARE CONTRACT:
//! `classifyFailure` · `renderDiagnosisMd` · `renderRecommendationMd`. The closed AI-failure taxonomy
//! + first-match-wins deterministic classifier (EPIC-AI-BEHAVIOR-STUDY).
// QUALITY: 85/100 — ported classifier + renderers verbatim; regexes are (?i) with no lookaround.
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// JS String(v) (no ??): null→"null", array→comma-join, object→"[object Object]".
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

// JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

// JS `${v ?? dflt}` — nullish falls back to dflt, else String(v).
fn tmpl_or(v: Option<&Value>, dflt: &str) -> String {
    match nz(v) { None => dflt.to_string(), Some(x) => js_string(x) }
}

// JS `x ?? []` narrowed to an array slice (non-array/nullish → empty; non-array isn't fixtured).
fn arr<'a>(v: Option<&'a Value>) -> &'a [Value] {
    match v { Some(Value::Array(a)) => a.as_slice(), _ => &[] }
}

// ── evidence text helpers ─────────────────────────────────────────────

fn evidence_text(evidence: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for e in arr(evidence.get("validation_errors")) { parts.push(js_string(e)); }
    if truthy(evidence.get("failure_md")) { parts.push(js_string(evidence.get("failure_md").unwrap())); }
    if truthy(evidence.get("failure_kind")) { parts.push(js_string(evidence.get("failure_kind").unwrap())); }
    for v in arr(evidence.get("lint_violations")) {
        if let Value::String(s) = v {
            parts.push(s.clone());
        } else {
            parts.push(format!("{} {}", tmpl_or(v.get("file"), ""), tmpl_or(v.get("problem"), "")));
        }
    }
    parts.join("\n")
}

fn provider_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)all providers failed|provider (?:error|unavailable)|adapter (?:error|failed)|ECONN|ETIMEDOUT|fetch failed|rate.?limit").unwrap()) }
fn feed_gap_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)feed quality|missing=\[|unfilled placeholder|placeholder .* (?:missing|empty)").unwrap()) }
fn invented_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)content after the closing fence|nothing belongs after|unknown section|stray prose|outside (?:the )?(?:form|sections)").unwrap()) }
fn format_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)form compile|unclosed (?:code )?fence|missing section|leftover placeholder|JSON|parse|fails pattern|missing required|not (?:a|an) (?:native )?array|invalid enum|expected object").unwrap()) }
fn byte_drift_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)byte-exact|byte-check|anchor dry-run|anchor (?:not found|miss)|MANDATORY .* (?:miss|absent)").unwrap()) }
fn vague_lint_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)vague phrase|no concrete structure|no versioned dependency|scripts map not defined").unwrap()) }
fn refusal_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)empty[_ ]refusal|refused|empty output").unwrap()) }

/// Files implicated by lint violations (for the dropped-decisions check).
fn violated_files(evidence: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for v in arr(evidence.get("lint_violations")) {
        let mapped: Option<&Value> = if v.is_string() { None } else { nz(v.get("file")) };
        if let Some(x) = mapped {
            if truthy(Some(x)) { out.push(x.clone()); } // .filter(Boolean)
        }
    }
    out
}

/// Did the paired reason leg decide concrete content for the violated files?
fn reason_had_decisions_for(evidence: &Value, files: &[Value]) -> bool {
    let decisions: Vec<&Value> = arr(evidence.get("reason_outputs")).iter().filter(|o| {
        o.get("field_name").and_then(|v| v.as_str()) == Some("file_decisions_list")
            && o.get("value_text").map_or(false, |v| v.is_string())
    }).collect();
    if decisions.is_empty() || files.is_empty() { return false; }
    files.iter().any(|f| {
        let needle = js_string(f);
        decisions.iter().any(|d| d.get("value_text").and_then(|v| v.as_str()).map_or(false, |vt| vt.contains(&needle)))
    })
}

// ── classifier ────────────────────────────────────────────────────────

pub fn classifyFailure(evidence: &Value) -> Value {
    let text = evidence_text(evidence);
    let out = match nz(evidence.get("raw_output_md")) { None => String::new(), Some(x) => js_string(x) };
    let tokens_out = match nz(evidence.get("tokens_out")) { None => f64::NAN, Some(x) => js_number(Some(x)) };
    let max_out = match nz(evidence.get("max_output_tokens")) { None => f64::NAN, Some(x) => js_number(Some(x)) };

    // 1. infra before content: a dead provider explains everything else.
    let outcome = evidence.get("outcome").and_then(|v| v.as_str());
    if outcome == Some("timeout") || outcome == Some("error") || provider_re().is_match(&text) {
        return json!("provider_error");
    }
    // 2. ceiling hit: content errors downstream of a cut-off answer are noise.
    if tokens_out.is_finite() && max_out.is_finite() && max_out > 0.0 && tokens_out >= 0.98 * max_out {
        return json!("truncation");
    }
    // 3. nothing came back to judge — but only when raw_output_md was actually supplied.
    if (nz(evidence.get("raw_output_md")).is_some() && out.trim().chars().count() < 40) || refusal_re().is_match(&text) {
        return json!("refusal_empty");
    }
    // 4. prompt-side bug — the model never saw a complete prompt.
    if feed_gap_re().is_match(&text) { return json!("feed_gap"); }
    // 5. more specific than generic format breakage.
    if invented_re().is_match(&text) { return json!("invented_structure"); }
    // 6. generic shape breakage.
    if format_re().is_match(&text) { return json!("format_noncompliance"); }
    // 7. plan-lint vagueness — split on whether the reason leg HAD decided.
    if !arr(evidence.get("lint_violations")).is_empty() || vague_lint_re().is_match(&text) {
        let files = violated_files(evidence);
        return if reason_had_decisions_for(evidence, &files) { json!("dropped_reason_decisions") } else { json!("vague_spec") };
    }
    // 8. byte-exact channel failures.
    if byte_drift_re().is_match(&text) { return json!("byte_drift"); }
    json!("validation_other")
}

// ── deterministic renderers ───────────────────────────────────────────

// JS clip(s, n): String(s ?? '').trim(); if length > n → `${slice(0,n)}\n…(N chars clipped)`.
fn clip(s: Option<&Value>, n: usize) -> String {
    let raw = match nz(s) { None => String::new(), Some(x) => js_string(x) };
    let t = raw.trim();
    let len = t.chars().count();
    if len > n {
        let head: String = t.chars().take(n).collect();
        format!("{}\n\u{2026}({} chars clipped)", head, len - n)
    } else {
        t.to_string()
    }
}

// DIAGNOSIS[cls] — all diagnosis fns ignore evidence, so this is a pure class→string map. The `_`
// arm is DIAGNOSIS.validation_other (the fallback when cls is not a known key).
fn diagnosis_head(key: &str) -> &'static str {
    match key {
        "vague_spec" => "The model emitted specifications with undecided values (vague phrases, versionless deps, or code specs naming no concrete structure). Natural language cannot fail to parse, so undecided values travel silently until the deterministic spec lint refuses them (doc-21 rule 3: fail at LINT, never at QA).",
        "dropped_reason_decisions" => "The reason leg (phase 1) DID decide concrete content for the offending files, but the format leg paraphrased those decisions away instead of copying them verbatim. This is a phase-handoff loss, not a thinking failure.",
        "invented_structure" => "The model appended structure the contract never asked for (extra sections/appendices outside the form). Doc-21 class: the model answering a human that isn't there \u{2014} it synthesized a ritual from the prompt's prose description of downstream checks.",
        "format_noncompliance" => "The answer broke the machine-readable shape (form skeleton, fences, JSON, or required sections). The deterministic compiler/linter refused it mechanically.",
        "byte_drift" => "Byte-exact data was paraphrased in transit (mandatory-line miss or anchor dry-run failure). Byte-exact content must ride a verbatim channel, never prose.",
        "truncation" => "The output hit the max_output_tokens ceiling \u{2014} everything after the cut is missing, and downstream validation errors are secondary to the truncation.",
        "refusal_empty" => "The model returned an empty or near-empty answer (refusal or null completion). The contract demands content; nothing arrived to validate.",
        "feed_gap" => "The prompt itself was incomplete (unfilled placeholders / feed-quality failure). This is a substrate-side defect: the model never received what the template promised.",
        "provider_error" => "The provider/adapter failed (timeout, connection, or all-providers-failed). No judgment about the model's behavior is possible from this call.",
        _ => "Validation failed for a reason outside the cataloged classes. Inspect the attempts trail and raw output; if a pattern repeats, extend the taxonomy.",
    }
}

pub fn renderDiagnosisMd(cls: &Value, evidence: &Value) -> Value {
    let key = js_string(cls);
    let head = diagnosis_head(&key);
    let mut lines: Vec<String> = vec![
        format!("**Failure class:** `{}`", key),
        String::new(),
        head.to_string(),
    ];
    let errs: Vec<&Value> = arr(evidence.get("validation_errors")).iter().take(5).collect();
    if !errs.is_empty() {
        lines.push(String::new());
        lines.push("**Validator said:**".to_string());
        for &e in &errs { lines.push(format!("- {}", clip(Some(e), 300))); }
    }
    let lint: Vec<&Value> = arr(evidence.get("lint_violations")).iter().take(8).collect();
    if !lint.is_empty() {
        lines.push(String::new());
        lines.push("**Lint violations:**".to_string());
        for &v in &lint {
            let item = if v.is_string() {
                clip(Some(v), 200)
            } else {
                format!("[{}] {}: {}", tmpl_or(v.get("microtask_id"), "?"), tmpl_or(v.get("file"), "?"), clip(v.get("problem"), 200))
            };
            lines.push(format!("- {}", item));
        }
    }
    if truthy(evidence.get("raw_output_md")) {
        lines.push(String::new());
        lines.push("**Raw output (excerpt):**".to_string());
        lines.push("```".to_string());
        lines.push(clip(evidence.get("raw_output_md"), 800));
        lines.push("```".to_string());
    }
    if truthy(evidence.get("failure_md")) {
        lines.push(String::new());
        lines.push("**Stage failure:**".to_string());
        lines.push(clip(evidence.get("failure_md"), 600));
    }
    json!(lines.join("\n"))
}

pub fn renderRecommendationMd(cls: &Value, evidence: &Value) -> Value {
    let key = js_string(cls);
    let pn = tmpl_or(evidence.get("prompt_name"), "the template");
    match key.as_str() {
        "vague_spec" => json!(format!("Tighten the SELF-LINT contract of {}: demand exact values where the lint fired, and verify the reason leg is being asked to DECIDE (file_decisions_list) before the format leg writes. Operator applies template edits.", pn)),
        "dropped_reason_decisions" => {
            let tier = tmpl_or(evidence.get("tier"), "low");
            json!(format!("Add a copy-verbatim directive to the format leg of {} (\"copy each file_decisions_list entry into its spec UNCHANGED\"), or bump the format leg {}\u{2192}mid via routing_defaults. Operator applies.", pn, tier))
        }
        "invented_structure" => json!(format!("State explicitly in {} where the answer ENDS and that downstream checks are executed by the substrate, never reported by the model. Consider compiler tolerance only if the invented tail is harmless.", pn)),
        "format_noncompliance" => json!(format!("Strengthen the form/skeleton rules of {} (fence guidance, required sections) and verify the compile errors carry line numbers back in the correction. If the same break repeats at low tier, bump to mid.", pn)),
        "byte_drift" => json!("Move byte-exact content into a verbatim channel (pre-filled form field or echo gate) instead of asking the model to restate it."),
        "truncation" => json!(format!("Raise max_output_tokens for {} or split the task (per-file authoring instead of whole-plan output).", pn)),
        "refusal_empty" => json!("Check the feed (were inputs empty?) and the task framing; if inputs were complete, escalate the tier \u{2014} refusals at low tier often vanish at mid."),
        "feed_gap" => json!("Fix the caller: a placeholder went unfilled or feed-quality flagged the inputs. This is substrate-side; no template/tier change will help."),
        "provider_error" => json!("Infra issue \u{2014} check provider status, failover order, and timeouts. Exclude these calls from prompt-quality statistics."),
        _ => Value::Null, // validation_other + any unknown class → RECOMMENDATION.validation_other → null
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cls(e: Value) -> String { classifyFailure(&e).as_str().unwrap().to_string() }

    #[test]
    fn classifier_first_match_wins() {
        assert_eq!(cls(json!({ "outcome": "timeout" })), "provider_error");
        assert_eq!(cls(json!({ "outcome": "error" })), "provider_error");
        assert_eq!(cls(json!({ "validation_errors": ["all providers failed for tier low"] })), "provider_error");
        assert_eq!(cls(json!({ "tokens_out": 3990, "max_output_tokens": 4000, "validation_errors": ["missing section ## CODE"] })), "truncation");
        assert_eq!(cls(json!({ "raw_output_md": "   " })), "refusal_empty");
        assert_eq!(cls(json!({ "validation_errors": ["the model refused to answer"] })), "refusal_empty");
        assert_eq!(cls(json!({ "validation_errors": ["feed quality check failed: missing=[briefing_excerpt]"] })), "feed_gap");
        assert_eq!(cls(json!({ "validation_errors": ["form compile: content after the closing fence (\"## X\")"] })), "invented_structure");
        assert_eq!(cls(json!({ "validation_errors": ["form compile: missing section ## CODE"] })), "format_noncompliance");
        assert_eq!(cls(json!({
            "lint_violations": [{ "microtask_id": "m", "file": "apps/x/vite.config.ts", "problem": "code-file spec names no concrete structure" }],
            "reason_outputs": [{ "field_name": "file_decisions_list", "value_text": "apps/x/vite.config.ts: import { defineConfig } from 'vite'" }]
        })), "dropped_reason_decisions");
        assert_eq!(cls(json!({ "lint_violations": [{ "file": "apps/x/App.css", "problem": "spec uses the vague phrase" }] })), "vague_spec");
        assert_eq!(cls(json!({ "failure_md": "plan spec lint failed: spec uses the vague phrase \"as required\"" })), "vague_spec");
        assert_eq!(cls(json!({ "failure_md": "byte-check: 2 MANDATORY lines miss in README.md" })), "byte_drift");
        assert_eq!(cls(json!({ "validation_errors": ["some never-seen condition"] })), "validation_other");
        // totality: garbage still lands in a class
        assert_eq!(cls(Value::Null), "validation_other");
        assert_eq!(cls(json!({})), "validation_other");
        assert_eq!(cls(json!({ "raw_output_md": "a sufficiently long output that clearly exceeds forty chars" })), "validation_other");
    }

    #[test]
    fn renderers_cite_class_and_recommend() {
        let md = renderDiagnosisMd(&json!("vague_spec"), &json!({ "validation_errors": ["x"], "lint_violations": ["str violation", {}] }));
        let s = md.as_str().unwrap();
        assert!(s.contains("**Failure class:** `vague_spec`"));
        assert!(s.contains("**Validator said:**"));
        assert!(s.contains("[?] ?: "));
        // unknown class falls back to the validation_other diagnosis body
        let un = renderDiagnosisMd(&json!("nope"), &json!({}));
        assert!(un.as_str().unwrap().contains("outside the cataloged classes"));
        // clip fires past the threshold
        let long = "a".repeat(810);
        let clipped = renderDiagnosisMd(&json!("truncation"), &json!({ "raw_output_md": long }));
        assert!(clipped.as_str().unwrap().contains("chars clipped)"));

        assert!(renderRecommendationMd(&json!("vague_spec"), &json!({ "prompt_name": "a.b.v1" })).as_str().unwrap().contains("a.b.v1"));
        assert!(renderRecommendationMd(&json!("dropped_reason_decisions"), &json!({})).as_str().unwrap().contains("the template"));
        assert_eq!(renderRecommendationMd(&json!("validation_other"), &json!({})), Value::Null);
        assert_eq!(renderRecommendationMd(&json!("unknown_class"), &json!({})), Value::Null);
    }
}
