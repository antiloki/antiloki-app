//! ROOT CAUSE (UI-19, operator: "a second pipeline branch only triggered if an error occurs"): when a run fails, look at what it left (the failure events, the node, the tries) and test a fixed list of causes against it.
//! A cause is CONFIRMED when the evidence says so (with the line it comes from), RULED OUT when the evidence says it is not that, and the finding goes to the improver as the problem to fix. No model is called and nothing is changed.
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hypothesis { pub claim: String, /** confirmed | ruled_out */ pub status: &'static str, pub evidence: String, /** what to change, when confirmed */ pub fix: String }
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding { pub run: String, pub node: String, pub kind: String, pub error: String, pub failures: usize, pub hypotheses: Vec<Hypothesis>, /** the problem statement for the improver; empty when the run did not fail */ pub finding: String }

/// A failure event's text is `<node> failed (<Kind>): <error>`.
fn parse_failure(text: &str) -> Option<(String, String, String)> {
    let (head, error) = text.split_once("): ")?; let (node, kind) = head.split_once(" failed (")?; Some((node.to_string(), kind.to_string(), error.to_string()))
}
fn any(s: &str, words: &[&str]) -> Option<String> { let l = s.to_lowercase(); words.iter().find(|w| l.contains(*w)).map(|w| w.to_string()) }

/// Test every cause against the failures of a run (`events`: the run's events, newest or oldest first), the latest failure being the one investigated. Pure.
pub fn investigate(run: &str, events: &[Value], label_of: &dyn Fn(&str) -> String) -> Finding {
    let fails: Vec<(String, String, String)> = events.iter().filter(|e| e["kind"] == "failed").filter_map(|e| parse_failure(e["text"].as_str().unwrap_or(""))).collect();
    // events come newest first from the store: the first failure listed is the latest
    let Some((node, kind, error)) = fails.first().cloned() else { return Finding { run: run.into(), node: String::new(), kind: String::new(), error: String::new(), failures: 0, hypotheses: vec![], finding: String::new() } };
    let same = fails.iter().filter(|f| f.0 == node).count(); let at = label_of(&node);
    let h = |claim: &str, hit: Option<String>, yes: String, no: &str, fix: &str| Hypothesis { claim: claim.into(), status: if hit.is_some() { "confirmed" } else { "ruled_out" }, evidence: if hit.is_some() { yes } else { no.into() }, fix: if hit.is_some() { fix.into() } else { String::new() } };
    let mut hs = vec![
        { let hit = (kind == "Timeout").then(|| "the failure kind is Timeout".to_string()).or_else(|| any(&error, &["timed out", "timeout"]).map(|w| format!("the error says \"{w}\""))); h("The step ran out of time", hit.clone(), hit.unwrap_or_default(), "the failure is not a timeout", "give the step more time, or split it into smaller steps") },
        { let hit = any(&error, &["quota", "rate limit", "limit reached", "usage limit"]); h("The seat was out of quota", hit.clone(), format!("the error says \"{}\"", hit.unwrap_or_default()), "the error does not mention a limit", "route the step to another seat, or wait for the window to reset") },
        { let hit = any(&error, &["command not found", "no such file", "not found", "enoent", "not installed", "cannot find"]); h("A tool or file the step needs is missing", hit.clone(), format!("the error says \"{}\"", hit.unwrap_or_default()), "the error names no missing tool or file", "install it, or say in the brief where the step finds it") },
        { let hit = (kind == "Cap" || kind == "Budget").then(|| format!("the failure kind is {kind}")); h("The step hit its spending limit", hit.clone(), hit.unwrap_or_default(), "the failure is not about spending", "raise the cap of the step, or make its brief smaller") },
        { let hit = (kind == "Verdict").then(|| "the failure kind is Verdict: the step gave no usable answer".to_string()); h("The step's answer was not usable", hit.clone(), hit.unwrap_or_default(), "the step did give an answer", "say in the brief the exact shape of the answer, or add a check that names what is wrong") },
        { let hit = any(&error, &["permission", "denied", "not allowed", "forbidden", "unauthorized", "401", "403"]); h("The step was not allowed to do something", hit.clone(), format!("the error says \"{}\"", hit.unwrap_or_default()), "the error mentions no refusal", "allow that effect for the step, or move the work to a step that may do it") },
        { let hit = (same >= 2).then(|| format!("{at} failed {same} times in this run")); h("It was retried without anything changing", hit.clone(), hit.unwrap_or_default(), "the step failed once", "retry with a stronger seat, or change what the step is told before it tries again") },
    ];
    hs.sort_by_key(|x| x.status != "confirmed");
    let confirmed: Vec<&Hypothesis> = hs.iter().filter(|x| x.status == "confirmed").collect();
    let finding = if confirmed.is_empty() { format!("Run {run} failed at \"{at}\" ({kind}): {}. None of the known causes fits the evidence: read the step's transcript.", error.chars().take(300).collect::<String>()) }
        else { format!("Run {run} failed at \"{at}\" ({kind}): {}.\n{}", error.chars().take(300).collect::<String>(), confirmed.iter().map(|x| format!("- {} ({}). To change: {}.", x.claim, x.evidence, x.fix)).collect::<Vec<_>>().join("\n")) };
    Finding { run: run.into(), node, kind, error, failures: fails.len(), hypotheses: hs, finding }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn ev(t: &str) -> Value { json!({ "kind": "failed", "text": t }) }
    #[test] fn the_causes_the_evidence_supports_are_confirmed_and_the_rest_ruled_out_and_the_finding_goes_to_the_improver() {
        let f = investigate("r1", &[ev("builder failed (Error): font Inter: command not found"), ev("builder failed (Error): font Inter: command not found")], &|n| if n == "builder" { "HyperFrames builder".into() } else { n.into() });
        assert_eq!(f.node, "builder"); assert_eq!(f.failures, 2);
        let on = |c: &str| f.hypotheses.iter().find(|h| h.claim.contains(c)).unwrap().status;
        assert_eq!(on("missing"), "confirmed"); assert_eq!(on("retried"), "confirmed"); assert_eq!(on("out of time"), "ruled_out"); assert_eq!(on("quota"), "ruled_out");
        assert_eq!(f.hypotheses[0].status, "confirmed", "confirmed ones come first");
        assert!(f.finding.contains("HyperFrames builder") && f.finding.contains("command not found") && f.finding.contains("To change:"), "{}", f.finding);
    }
    #[test] fn a_timeout_is_a_timeout_and_an_unknown_error_says_so_and_a_clean_run_has_no_finding() {
        assert_eq!(investigate("r", &[ev("a failed (Timeout): the step took too long")], &|n| n.into()).hypotheses.iter().find(|h| h.claim.contains("out of time")).unwrap().status, "confirmed");
        let u = investigate("r", &[ev("a failed (Error): weird")], &|n| n.into()); assert!(u.hypotheses.iter().all(|h| h.status == "ruled_out") && u.finding.contains("None of the known causes"));
        let none = investigate("r", &[json!({ "kind": "done", "text": "ok" })], &|n| n.into()); assert!(none.finding.is_empty() && none.hypotheses.is_empty());
    }
}
