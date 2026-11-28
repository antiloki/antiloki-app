//! THE IMPROVER'S TWO HANDS (RFC 0013 §3.1-3.2, step 7): the text it reads (evidence, TEAM.md, what the person refused before) and the turning of its answer into a request. The serve never calls a model here:
//! the improver is an agent (or the Leader) that reads `prompt` and answers; `request_from_reply` vets that answer and opens ONE request, or says why not.
use crate::team_graph::Graph;

pub fn prompt(root: &str, team: &str, g: &Graph) -> String {
    let notes = std::fs::read_to_string(crate::team_repo::repo_dir(root, team).join("TEAM.md")).unwrap_or_default();
    let refused = crate::team_request::refusals(root, team);
    format!("You are the IMPROVER of an AI team. You change nothing yourself: you propose ONE small change as a request the person reads.\n\n{}\nTEAM.MD (how the team works)\n{notes}\nTHE PERSON REFUSED BEFORE (do not propose these again)\n{}\n\nRULES\n- Read the last runs and name ONE recurring problem, with the run ids that show it, in the `evidence` field.\n- ONE idea, at most {} small differences to the recipe. Prompts, order, models, loop limits, adding or dropping a node. Never spend limits, gates, checks, approvals, triggers or secrets.\n- Answer with JSON only: {{\"proposals\": [{{\"node\": \"<id>\", \"kind\": \"prompt_wording|other\", \"patch\": [<ops>], \"hypothesis\": \"the problem and why this fixes it\", \"metric\": \"the number that should move\", \"evidence\": \"run ids and what they show\"}}]}}\n",
        crate::team_improve::brief(root, team, g), if refused.is_empty() { "nothing yet".to_string() } else { refused.join("\n") }, crate::team_request::MAX_CHANGES)
}

/// The improver's answer to a request. The first proposal that holds up opens a branch; none that does says why.
pub fn request_from_reply(root: &str, team: &str, reply: &str, run_ids: &[String], now_ms: i64) -> Result<crate::team_request::Meta, String> {
    let live = crate::team_graph_run::load_graph(root, team)?;
    let ps = crate::team_improve::parse_proposals(reply)?; let mut why = vec![];
    for p in ps {
        if p.patch.is_empty() || p.hypothesis.trim().is_empty() || p.evidence.trim().is_empty() { why.push("a proposal needs a patch, a hypothesis and its evidence".to_string()); continue; }
        if crate::team_patch::patch_requires_person(&p.patch, &live) && p.risk == "low" { why.push("it says low risk but touches something only the person decides".into()); continue; }
        let cand = match crate::team_patch::apply_ops(&live, &p.patch) { Ok(g) => g, Err(e) => { why.push(e); continue; } };
        match crate::team_request::open(root, &live, &cand, &format!("{} (evidence: {})", p.hypothesis, p.evidence), run_ids, now_ms) { Ok(m) => return Ok(m), Err(e) => why.push(e) }
    }
    Err(if why.is_empty() { "the answer held no proposal".into() } else { why.join("; ") })
}
