//! CONFIDENCE AFTER THE RESULT (operator 2026-10-10: "QA is optional, not obligatory; confidence is displayed after the results and a suggestion to run independent QA can be shown like a button").
//!
//! A run no longer has to pay for a QA pass to be trusted or not: when it ends, the facts it already holds are turned into one of three words and the reasons behind it, and when QA did not run the
//! result offers to run it. The reading is deterministic and says only what it can point at (the gauges' rule): a deterministic check that passed, a verifier's verdict, an independent QA's verdict,
//! the attempts it took, design rules that failed. No percentage is invented. Pure - tested.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Confidence {
    /// "high" | "medium" | "low"
    pub level: String,
    /// plain-language reasons, strongest first
    pub reasons: Vec<String>,
    /// an independent QA (the claim gate and the visual evidence) has judged this run
    pub qa_ran: bool,
    /// the result may offer "run independent QA": QA did not run and the run has finished work to judge
    pub suggest_qa: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub write: bool,
    pub tasks: usize,
    pub failed: usize,
    pub changed_files: usize,
    /// attempts beyond the first, over all tasks
    pub retries: u32,
    /// the compiler or the project's own tests passed on the changed files (a machine said so)
    pub machine_proof: bool,
    pub verifier_met: usize,
    pub verifier_unmet: usize,
    pub qa_ran: bool,
    pub qa_met: Option<bool>,
    pub design_blockers: usize,
}

pub fn confidence_of(f: &Facts) -> Confidence {
    let suggest_qa = !f.qa_ran && f.write && f.tasks > 0 && f.failed == 0;
    let low = |why: &str| Confidence { level: "low".into(), reasons: vec![why.to_string()], qa_ran: f.qa_ran, suggest_qa };
    if f.tasks == 0 || f.failed > 0 { return low("a task did not finish") }
    if f.write && f.changed_files == 0 { return low("nothing was changed") }
    if f.verifier_unmet > 0 { return low("the verifier judged a claim not met") }
    if f.qa_ran && f.qa_met == Some(false) { return low("an independent QA found a claim not met") }
    let (mut pts, mut reasons) = (0i32, vec![]);
    if f.machine_proof { pts += 2; reasons.push("the compiler or the project's tests passed on the changed files".to_string()); }
    if f.qa_ran && f.qa_met == Some(true) { pts += 2; reasons.push("an independent QA judged it met".to_string()); }
    if f.verifier_met > 0 { pts += 1; reasons.push("the verifier judged the claims met".to_string()); }
    if f.retries == 0 { pts += 1; reasons.push("it passed on the first attempt".to_string()); }
    if f.retries >= 2 { pts -= 1; reasons.push(format!("it needed {} retries", f.retries)); }
    if f.design_blockers > 0 { pts -= 2; reasons.push(format!("{} design rule(s) failed (contrast, overflow or clipping)", f.design_blockers)); }
    if !f.machine_proof && !f.qa_ran { reasons.push("no deterministic check ran: the verifier's judgement stands alone".to_string()); }
    let level = if pts >= 3 { "high" } else if pts == 2 { "medium" } else { "low" };
    Confidence { level: level.into(), reasons, qa_ran: f.qa_ran, suggest_qa }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base() -> Facts { Facts { write: true, tasks: 1, changed_files: 2, verifier_met: 1, ..Default::default() } }

    #[test]
    fn a_machine_proof_on_the_first_attempt_is_high_and_a_lone_verifier_is_medium() {
        let proof = Facts { machine_proof: true, ..base() };
        let c = confidence_of(&proof); assert_eq!(c.level, "high", "{c:?}"); assert!(c.suggest_qa && !c.qa_ran);
        let lone = confidence_of(&base()); assert_eq!(lone.level, "medium", "{lone:?}");
        assert!(lone.reasons.iter().any(|r| r.contains("verifier's judgement stands alone")), "a lone verdict says so");
    }
    #[test]
    fn retries_and_design_failures_pull_it_down_and_failure_is_low() {
        assert_eq!(confidence_of(&Facts { retries: 2, ..base() }).level, "low", "verifier + two retries: 1 + 0 - 1 = 0");
        assert_eq!(confidence_of(&Facts { machine_proof: true, design_blockers: 1, ..base() }).level, "medium", "a failing design rule costs a step");
        assert_eq!(confidence_of(&Facts { failed: 1, ..base() }).level, "low");
        assert_eq!(confidence_of(&Facts { changed_files: 0, ..base() }).level, "low", "a write run that changed nothing");
        assert_eq!(confidence_of(&Facts { verifier_unmet: 1, machine_proof: true, ..base() }).level, "low");
    }
    #[test]
    fn an_independent_qa_decides_and_the_button_is_offered_only_when_qa_did_not_run() {
        let met = confidence_of(&Facts { qa_ran: true, qa_met: Some(true), ..base() });
        assert_eq!(met.level, "high"); assert!(!met.suggest_qa && met.qa_ran);
        let unmet = confidence_of(&Facts { qa_ran: true, qa_met: Some(false), machine_proof: true, ..base() });
        assert_eq!(unmet.level, "low"); assert!(!unmet.suggest_qa);
        assert!(!confidence_of(&Facts { failed: 1, ..base() }).suggest_qa, "nothing finished: nothing to judge");
        assert!(!confidence_of(&Facts { write: false, ..base() }).suggest_qa, "a read-only run wrote nothing to judge");
    }
}
