//! bugfinder.rs — twin of skills/engine/canonical-actions/bugfinder.mjs: DATA statics pressed by the crossing + the
//! hand-ported accessor. NAMES ARE CONTRACT: statics and fns carry exact JS names.
// QUALITY: 80/100 — pressed statics + trivial hand accessor; blob unreadable by design, surface fully tested
use serde_json::Value;
use std::sync::LazyLock;

pub static ACTIONS: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"[{"name":"system.bugfinder.run","description":"BUG FINDER — the 3-wave defect gate the pipeline never had: wave 1 deterministic oracles ($0: lint · typecheck · import/symbol existence · atlas smells∩scope), wave 2 narrow low-tier shards per bug species (checking · algorithm · timing · interface · security · test-oracle; every lead needs a verbatim quote or is void), wave 3 ONE mid-tier sweep for anything outside the covered categories. ADVISORY: records findings, never blocks. input: {files?|base?, shards?, skip?, source?, session_id?, contract_slug?, max_files?}.","owner_agent":"system","steps_json":"[{\"skill\":\"bugfinder-run\",\"input\":\"$.input\",\"as\":\"result\"}]","returns":"$.results.result","on_failure":"abort"},{"name":"system.bugfinder.report","description":"BUG FINDER / ULTRA REVIEW dashboard payload: runs, category/severity/source breakdowns, the operator-classification precision ledger, and schedules with computed due state. Read-only. input: {run_id?, days?}.","owner_agent":"system","steps_json":"[{\"skill\":\"bugfinder-report\",\"input\":\"$.input\",\"as\":\"result\"}]","returns":"$.results.result","on_failure":"abort"},{"name":"system.bugfinder.classify","description":"BUG FINDER one-click classify (tagging doctrine): stamp a finding real | false-positive | known (null clears). Feeds the precision ledger — the only advisory→gating promotion path. input: {finding_id, class}.","owner_agent":"system","steps_json":"[{\"skill\":\"bugfinder-classify\",\"input\":\"$.input\",\"as\":\"result\"}]","returns":"$.results.result","on_failure":"abort"},{"name":"system.bugfinder.schedule","description":"ULTRA REVIEW scheduler: save/delete recurring bug-finder runs over a saved scope (folder · files · diff base; daily/weekly), run one now, or run every overdue one. input: {op: save|delete|run|run-due, …}.","owner_agent":"system","steps_json":"[{\"skill\":\"bugfinder-schedule\",\"input\":\"$.input\",\"as\":\"result\"}]","returns":"$.results.result","on_failure":"abort"}]"#).expect("ACTIONS: embedded JSON is valid by construction")
});

/// JS: `bugfinderActions(_input, _context)` — returns the canonical data, verbatim.
pub fn bugfinderActions(_input: &Value, _context: &Value) -> Value {
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
    fn bugfinderactions_returns_the_static_verbatim() {
        assert_eq!(bugfinderActions(&Value::Null, &Value::Null), *ACTIONS);
    }

}
