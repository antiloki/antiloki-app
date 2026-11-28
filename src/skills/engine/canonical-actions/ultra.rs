//! ultra.rs — twin of skills/engine/canonical-actions/ultra.mjs: DATA statics pressed by the crossing + the
//! hand-ported accessor. NAMES ARE CONTRACT: statics and fns carry exact JS names.
// QUALITY: 80/100 — pressed statics + trivial hand accessor; blob unreadable by design, surface fully tested
use serde_json::Value;
use std::sync::LazyLock;

pub static ACTIONS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"[{"name":"system.ultra.run","description":"ULTRA REVIEW survey — the deterministic score substrate (plan build order 1+3, $0, ZERO AI): scores every code file in a scope from counted signals (atlas metrics + smells, import closure, git churn/authors, bugfinder ledger) with visible versioned formulas (axes_json), cohort percentiles, blast-radius folder roll-ups, and the Determinism + AI-Readability meta scores. Advisory, never gating. input: {folder?|files?, max_files?, source?}.","owner_agent":"system","steps_json":"[{\"skill\":\"ultra-run\",\"input\":\"$.input\",\"as\":\"result\"}]","returns":"$.results.result","on_failure":"abort"},{"name":"system.ultra.deep","description":"ULTRA DEEP REVIEW — the staff-level audit (plan build order 4-6, pyramid order): full 3-wave bench sweep over the territory (verified findings feed the defect axis) → $0 survey re-score → the CONCEPT ATLAS (one low-tier call per file, cached by content_hash, near-duplicate ideas merged via local embeddings — surfaces scattered/untested/mixed ideas no call graph can see) → ONE high-tier dossier per worst-decile unit (why · alternatives · fix sketch · estimated gain, grounded in its own signals/findings/source, parse-or-drop) → ONE high-tier executive brief over everything incl. the concept atlas and the deltas since the last review. Advisory forever; spend lands in brain.ai_calls + the run totals. input: {folder?|files?, max_files?=120, shards?, skip?:[bench|concepts|dossiers|brief], dossier_cap?=8}.","owner_agent":"system","steps_json":"[{\"skill\":\"ultra-deep\",\"input\":\"$.input\",\"as\":\"result\"}]","returns":"$.results.result","on_failure":"abort"},{"name":"system.ultra.provenance","description":"ULTRA REVIEW provenance chain (root cause as data, $0, zero AI): for one file — the commits that shaped it → the card slugs those commits name → those cards + their contracts → the decisions recorded around them. Unresolved links are reported missing, never inferred. Pure reads. input: {path, limit?}.","owner_agent":"system","steps_json":"[{\"skill\":\"ultra-provenance\",\"input\":\"$.input\",\"as\":\"result\"}]","returns":"$.results.result","on_failure":"abort"},{"name":"system.ultra.report","description":"ULTRA REVIEW workspace payload: the scored territory of a survey run — summary + meta scores, blast-radius-weighted folder roll-ups with worst child, worst-unit rankings, and per-unit rows (signals + the visible axes formulas) for the treemap/dossier. Pure reads. input: {run_id?}.","owner_agent":"system","steps_json":"[{\"skill\":\"ultra-report\",\"input\":\"$.input\",\"as\":\"result\"}]","returns":"$.results.result","on_failure":"abort"}]"#).expect("ACTIONS: embedded JSON is valid by construction")
});

/// JS: `ultraActions(_input, _context)` — returns the canonical data, verbatim.
pub fn ultraActions(_input: &Value, _context: &Value) -> Value {
    ACTIONS.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn actions_parses_with_the_pressed_shape() {
        let v: &Value = &ACTIONS;
        let a = v.as_array().expect("array");
        assert_eq!(a.len(), 4, "pressed length");
        let mut names: Vec<&str> = a.iter().map(|x| x["name"].as_str().expect("name")).collect();
        let n = names.len();
        names.sort(); names.dedup();
        assert_eq!(names.len(), n, "names unique");
    }

    #[test]
    fn ultraactions_returns_the_static_verbatim() {
        assert_eq!(ultraActions(&Value::Null, &Value::Null), *ACTIONS);
    }

}
