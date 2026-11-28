//! get-pipelines-and-phases.rs — twin of skills/driver/get-pipelines-and-phases.mjs (db WRITE
//! lane — standard (input,context) signature, though the default export takes ZERO JS parameters
//! and touches no db: it's a pure fold over two sibling DATA modules). NAMES ARE CONTRACT:
//! `getPipelinesAndPhases` (default), `lookupRulePipeline` (named).
//!
//! RULE_ANNOTATIONS is reused verbatim from the already-converted twin
//! driver_pipeline_name_annotations::RULE_ANNOTATIONS. RULES itself has no Rust twin as DATA —
//! state-machine-rules.rs ports only the evaluateRules LOGIC (see that file's header: the raw
//! RULES array is `class:"transitive"` there, since stateMachineRules() returning function-valued
//! match/out fields verbatim has no byte-identical-transcription value). getPipelinesAndPhases
//! only ever reads `rule.name`, so RULE_NAMES below re-transcribes just that one field, in the
//! ORIGINAL RULES (v1) array order — 69 entries. Drift is caught two ways: the length assert
//! against RULE_ANNOTATIONS (which the JS test `annotations reference only real rule names`
//! keeps 1:1 with RULES) and the `zero tagging gaps` JS test asserting unclassified===0.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::driver_pipeline_name_annotations::RULE_ANNOTATIONS;

const RULE_NAMES: &[&str] = &[
    "repeat-failure-escalate",
    "card-done",
    "card-blocked",
    "snippet-human-gate",
    "card-has-children-awaiting",
    "skip-pl-route-blocked-too-big",
    "skip-pl-route",
    "pl-start",
    "pl-awaiting-user-start",
    "pl-clarifying-awaiting-answers",
    "pl-clarifying",
    "pl-researching",
    "pl-deciding-split",
    "pl-tuning",
    "pl-awaiting-approval",
    "pl-awaiting-approval-hold",
    "pl-done-no-contract",
    "contract-rejected-escalate-after-too-many",
    "contract-rejected-redo-operator-restart",
    "contract-rejected-redo",
    "contract-proposed",
    "swarm-needs-manifest-draft",
    "swarm-needs-manifest-lint",
    "swarm-manifest-lint-failed-redesign",
    "swarm-needs-author",
    "swarm-composition-failed-reauthor",
    "swarm-needs-review",
    "swarm-needs-coverage",
    "swarm-coverage-failed-reauthor",
    "swarm-stuck-escalate",
    "contract-pl-ratified-needs-plan",
    "contract-planned-needs-plan-coverage",
    "contract-plan-coverage-failed-retry",
    "contract-planned-needs-plan-review",
    "contract-planned-needs-snippets",
    "contract-pl-ratified",
    "contract-tl-approved-high-risk",
    "contract-tl-approved-needs-pl-review",
    "contract-tl-approved-needs-qa-review",
    "contract-tl-approved-qa-review-contract-level-escalate",
    "contract-tl-approved-qa-review-failed-retry",
    "contract-tl-approved-qa-review-failed-escalate",
    "contract-tl-approved-needs-man-phase1",
    "contract-tl-approved-needs-worktree",
    "contract-tl-approved-no-sandbox",
    "contract-tl-approved",
    "contract-human-approved",
    "contract-impl-needs-build-gate",
    "contract-impl-build-gate-failed-retry",
    "contract-impl-build-gate-failed-escalate",
    "contract-impl-no-qa",
    "contract-impl-qa-escalation-reason-set",
    "contract-impl-qa-failed-no-more-work-escalate",
    "contract-impl-qa-failed-amendment-to-tl",
    "contract-impl-amendment-cap-reached-escalate",
    "contract-impl-qa-failed-retry",
    "contract-impl-qa-failed-escalate",
    "contract-qa",
    "contract-done-needs-pr",
    "contract-done-pr-pending",
    "contract-done-needs-worktree-merge",
    "contract-done-needs-man-review",
    "contract-done-needs-keeper-consolidate",
    "contract-done-needs-journey-append",
    "contract-done-needs-review-blueprint",
    "contract-done-needs-atlas-integrate",
    "contract-done-card-needs-close",
    "contract-done",
    "contract-rejected-or-superseded",
];

struct PhaseGroup {
    phase: String,
    phase_order: Value,
    rule_names: Vec<String>,
}
struct PipelineGroup {
    name: String,
    phases: Vec<PhaseGroup>,
}

pub fn getPipelinesAndPhases(_input: &Value, _db: &Connection) -> Result<Value, String> {
    let mut pipelines: Vec<(String, PipelineGroup)> = Vec::new();
    let mut tagged: i64 = 0;
    let mut unclassified: i64 = 0;

    for &rule_name in RULE_NAMES {
        let annot = RULE_ANNOTATIONS.get(rule_name);
        let pipeline_name = annot
            .and_then(|a| a.get("pipeline_name"))
            .and_then(Value::as_str)
            .unwrap_or("unclassified")
            .to_string();
        let phase = annot
            .and_then(|a| a.get("phase"))
            .and_then(Value::as_str)
            .unwrap_or("unclassified")
            .to_string();
        let phase_order = annot.and_then(|a| a.get("phase_order")).cloned().unwrap_or_else(|| json!(999));
        if annot.is_some() { tagged += 1; } else { unclassified += 1; }

        let p_idx = match pipelines.iter().position(|(k, _)| k == &pipeline_name) {
            Some(i) => i,
            None => {
                pipelines.push((pipeline_name.clone(), PipelineGroup { name: pipeline_name.clone(), phases: Vec::new() }));
                pipelines.len() - 1
            }
        };
        let phases = &mut pipelines[p_idx].1.phases;
        let ph_idx = match phases.iter().position(|p| p.phase == phase) {
            Some(i) => i,
            None => {
                phases.push(PhaseGroup { phase: phase.clone(), phase_order, rule_names: Vec::new() });
                phases.len() - 1
            }
        };
        phases[ph_idx].rule_names.push(rule_name.to_string());
    }

    // Sort phases within each pipeline by phase_order then phase name (localeCompare); sort
    // rule_names within each phase alphabetically.
    for (_k, pl) in pipelines.iter_mut() {
        pl.phases.sort_by(|a, b| {
            let ao = a.phase_order.as_f64().unwrap_or(0.0);
            let bo = b.phase_order.as_f64().unwrap_or(0.0);
            ao.partial_cmp(&bo).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.phase.cmp(&b.phase))
        });
        for ph in pl.phases.iter_mut() {
            ph.rule_names.sort();
        }
    }

    let pipeline_count = pipelines.len() as i64;
    let total_rules = RULE_NAMES.len() as i64;

    let mut pipelines_obj = Map::new();
    for (k, pl) in pipelines {
        let phases_json: Vec<Value> = pl.phases.into_iter().map(|ph| {
            json!({ "phase": ph.phase, "phase_order": ph.phase_order, "rule_names": ph.rule_names })
        }).collect();
        pipelines_obj.insert(k, json!({ "name": pl.name, "phases": phases_json }));
    }

    Ok(json!({
        "pipelines": Value::Object(pipelines_obj),
        "summary": {
            "total_rules": total_rules,
            "tagged": tagged,
            "unclassified": unclassified,
            "pipeline_count": pipeline_count
        }
    }))
}

pub fn lookupRulePipeline(rule_name: &Value) -> Value {
    let key = rule_name.as_str().unwrap_or("");
    RULE_ANNOTATIONS.get(key).cloned().unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> Value {
        getPipelinesAndPhases(&Value::Null, &Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn total_rules_matches_rule_names_len() {
        let r = call();
        assert_eq!(r["summary"]["total_rules"], json!(69));
        assert_eq!(RULE_NAMES.len(), 69);
    }

    #[test]
    fn zero_tagging_gaps() {
        let r = call();
        assert_eq!(r["summary"]["unclassified"], json!(0));
        assert_eq!(r["summary"]["tagged"], json!(69));
    }

    #[test]
    fn all_five_expected_pipelines_exist() {
        let r = call();
        for expected in ["card_lifecycle", "pl_session", "contract_lifecycle", "recovery", "daemon_safety"] {
            assert!(r["pipelines"].get(expected).is_some(), "missing pipeline {expected}");
        }
        assert_eq!(r["summary"]["pipeline_count"], json!(5));
    }

    #[test]
    fn contract_lifecycle_is_the_largest() {
        let r = call();
        let count = |name: &str| -> i64 {
            r["pipelines"][name]["phases"].as_array().unwrap().iter()
                .map(|p| p["rule_names"].as_array().unwrap().len() as i64).sum()
        };
        let lifecycle_count = count("contract_lifecycle");
        for k in ["card_lifecycle", "pl_session", "recovery", "daemon_safety"] {
            assert!(count(k) <= lifecycle_count, "{k} exceeds contract_lifecycle");
        }
    }

    #[test]
    fn phases_sorted_by_phase_order_then_name() {
        let r = call();
        for pl in r["pipelines"].as_object().unwrap().values() {
            let phases = pl["phases"].as_array().unwrap();
            for w in phases.windows(2) {
                let ao = w[0]["phase_order"].as_f64().unwrap();
                let bo = w[1]["phase_order"].as_f64().unwrap();
                assert!(ao < bo || (ao == bo && w[0]["phase"].as_str() <= w[1]["phase"].as_str()));
            }
        }
    }

    #[test]
    fn rule_names_sorted_alphabetically_within_phase() {
        let r = call();
        for pl in r["pipelines"].as_object().unwrap().values() {
            for ph in pl["phases"].as_array().unwrap() {
                let names: Vec<&str> = ph["rule_names"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
                let mut sorted = names.clone();
                sorted.sort();
                assert_eq!(names, sorted);
            }
        }
    }

    #[test]
    fn recovery_pipeline_ties_at_phase_order_1_sort_by_phase_name() {
        // dev_retry, swarm_reauthor, swarm_redesign, tl_reauthor, tl_redraft, tl_replan all share
        // phase_order=1 in the recovery pipeline — a real tie requiring the localeCompare tiebreak.
        let r = call();
        let recovery = &r["pipelines"]["recovery"]["phases"];
        let names: Vec<&str> = recovery.as_array().unwrap().iter()
            .filter(|p| p["phase_order"].as_f64() == Some(1.0))
            .map(|p| p["phase"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["dev_retry", "swarm_reauthor", "swarm_redesign", "tl_reauthor", "tl_redraft", "tl_replan"]);
    }

    #[test]
    fn lookup_known_and_unknown_rule() {
        let known = lookupRulePipeline(&json!("contract-proposed"));
        assert_eq!(known["pipeline_name"], json!("contract_lifecycle"));
        let unknown = lookupRulePipeline(&json!("this-rule-does-not-exist"));
        assert_eq!(unknown, Value::Null);
    }
}
