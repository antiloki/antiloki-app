//! driver-doc-html.rs — twin of skills/render/driver-doc-html.mjs (standard lane; pure docs, no db).
//! NAMES ARE CONTRACT: `driverDocHtml` (default). Renders the driver orchestration-loop reference page.
//!
//! `RULES` (69 entries, `skills/driver/state-machine-rules.mjs`) is transcribed below as plain data.
//! This is safe here — unlike `state-machine-rules.rs`'s own `stateMachineRules()` (left
//! `class:"transitive"` because THAT consumer returns RULES verbatim, closures and all) —
//! because driver-doc-html.mjs's `tagFor`/`renderRulesTable` only ever read `rule.name` and
//! `rule.out.{next_agent,next_action,reason,terminal}` as plain PROPERTY ACCESS; `rule.match` is
//! never touched. One rule (`repeat-failure-escalate`) has a FUNCTION-valued `out` in JS (its
//! reason depends on `facts`, computed only when actually invoked as a rule). Since
//! driver-doc-html.mjs never CALLS `rule.out` — only reads properties off it — every property
//! read on that function value is `undefined` (JS functions don't carry `.terminal`/etc. unless
//! explicitly assigned), so it renders as tag="work" (neither `.terminal` nor `.next_action===null`
//! matches), agent/action="—", reason="" (`escHtml(undefined)` = `''`). Represented here as
//! `RuleOut::FnValued`, handled explicitly in `tag_for` + `render_rules_table_lines`.
//!
//! Verified byte-exact against a live `node` run of the JS source (all 69 rows + full doc
//! structure incl. the CSS/NAV constants and the escaped-backtick `db.exec(\`...\`)` example).
//! Frozen clock (2026-01-01T00:00:00.000Z) stands in for `new Date().toISOString()`.
use rusqlite::Connection;
use serde_json::Value;

fn generated_at() -> String { crate::clock::now_iso_nodb() }
const MAX_DEV_RETRIES: i64 = 3;

fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

// CSS constant — byte-exact value of the JS `const CSS` template (leading + trailing "\n").
const CSS: &str = "\n:root { --bg:#0e1116; --panel:#161b22; --panel2:#1f2530; --border:#30363d;\n        --fg:#e6edf3; --muted:#8b949e; --accent:#58a6ff; --good:#3fb950;\n        --warn:#d29922; --bad:#f85149; --code-bg:#0d1117; }\n* { box-sizing:border-box; }\nbody { font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif; background:var(--bg); color:var(--fg); margin:0; padding:0; line-height:1.55; }\n.wrap { max-width:1080px; margin:0 auto; padding:2em 1.5em 4em; }\nh1 { font-size:1.9em; font-weight:700; letter-spacing:-0.02em; margin:0 0 0.2em; }\nh2 { font-size:1.35em; font-weight:600; margin-top:2.4em; padding-bottom:0.3em; border-bottom:1px solid var(--border); }\nh3 { font-size:1.05em; font-weight:600; margin-top:1.6em; color:var(--fg); }\n.kicker { color:var(--accent); font-family:ui-monospace,monospace; font-size:0.78em; text-transform:uppercase; letter-spacing:0.08em; margin-bottom:0.2em; }\n.lede { color:var(--muted); font-size:1.02em; max-width:680px; }\n.meta { color:var(--muted); font-size:0.82em; margin:0.3em 0 1.5em; font-family:ui-monospace,monospace; }\ncode, pre { font-family:ui-monospace,\"JetBrains Mono\",Consolas,monospace; font-size:0.86em; }\ncode { background:var(--code-bg); padding:1px 5px; border-radius:3px; border:1px solid var(--border); }\npre { background:var(--code-bg); padding:0.9em 1.1em; border-radius:6px; border:1px solid var(--border); overflow-x:auto; }\npre code { background:none; border:none; padding:0; }\ntable { width:100%; border-collapse:collapse; margin:0.6em 0 1.5em; font-size:0.88em; }\nth, td { padding:0.55em 0.8em; text-align:left; border-bottom:1px solid var(--border); vertical-align:top; }\nth { color:var(--muted); font-weight:500; font-size:0.82em; background:var(--panel); }\ntr:hover td { background:var(--panel2); }\n.tag { padding:1px 7px; border-radius:10px; font-size:0.78em; font-family:ui-monospace,monospace; }\n.tag.term { background:rgba(248,81,73,0.15); color:var(--bad); border:1px solid rgba(248,81,73,0.4); }\n.tag.work { background:rgba(63,185,80,0.15); color:var(--good); border:1px solid rgba(63,185,80,0.4); }\n.tag.pause { background:rgba(210,153,34,0.15); color:var(--warn); border:1px solid rgba(210,153,34,0.4); }\n.panel { background:var(--panel); border:1px solid var(--border); border-radius:6px; padding:0.9em 1.1em; margin:0.8em 0; }\n.panel.warn   { border-color:var(--warn); }\n.panel.good   { border-color:var(--good); }\nul { padding-left:1.4em; }\nli { margin:0.3em 0; }\nhr { border:0; border-top:1px solid var(--border); margin:2em 0; }\n.muted { color:var(--muted); }\n";

// NAV constant — byte-exact value of the JS `const NAV` template (single line, verbatim).
const NAV: &str = r#"<nav style="padding:0.55em 1.5em;background:#161b22;border-bottom:1px solid #30363d;font-family:-apple-system,BlinkMacSystemFont,&quot;Segoe UI&quot;,sans-serif;font-size:0.85em;"><a href="./index.html" style="color:#58a6ff;text-decoration:none;">← antiloki docs</a></nav>"#;

enum RuleOut {
    // (next_agent, next_action, reason, terminal) — mirrors the JS object-literal `out`.
    Obj(Option<&'static str>, Option<&'static str>, &'static str, bool),
    // `out` is a function in JS (see module doc) — every property read is `undefined`.
    FnValued,
}

struct Rule {
    name: &'static str,
    out: RuleOut,
}

// RULES — transcribed verbatim (name + out shape) from skills/driver/state-machine-rules.mjs,
// in original array order. 69 entries; `RULES.length` in the doc comes from `.len()` below.
static RULES: &[Rule] = &[
    Rule { name: "repeat-failure-escalate", out: RuleOut::FnValued },
    Rule { name: "card-done", out: RuleOut::Obj(None, None, "card closed", true) },
    Rule { name: "card-blocked", out: RuleOut::Obj(None, None, "card blocked — human review needed", true) },
    Rule { name: "snippet-human-gate", out: RuleOut::Obj(None, None, "snippet human-gate: snippets authored → awaiting human approve / reject / edit", false) },
    Rule { name: "card-has-children-awaiting", out: RuleOut::Obj(None, None, "card already decomposed — work happens on children", false) },
    Rule { name: "skip-pl-route-blocked-too-big", out: RuleOut::Obj(Some("pl"), Some("pl.session.start"), "skip_pl=1 set but card size > S — re-routing through PL for decomposition (size-cascade enforcement)", false) },
    Rule { name: "skip-pl-route", out: RuleOut::Obj(Some("tl"), Some("contract.draft.auto"), "skip_pl_flag=1 (god-set) → bypass PL, route straight to contract draft", false) },
    Rule { name: "pl-start", out: RuleOut::Obj(Some("pl"), Some("pl.session.start"), "no PL session yet for this user-prioritized backlog card", false) },
    Rule { name: "pl-awaiting-user-start", out: RuleOut::Obj(None, None, "card not user-prioritized — type `do <slug>` in chat to start", false) },
    Rule { name: "pl-clarifying-awaiting-answers", out: RuleOut::Obj(Some("pl"), None, "PL clarifying with unanswered questions — awaiting human input", false) },
    Rule { name: "pl-clarifying", out: RuleOut::Obj(Some("pl"), Some("pl.session.advance.clarify"), "PL clarifying → run clarify step (clarifying → researching)", false) },
    Rule { name: "pl-researching", out: RuleOut::Obj(Some("pl"), Some("pl.session.advance.research"), "PL researching → continue research step", false) },
    Rule { name: "pl-deciding-split", out: RuleOut::Obj(Some("pl"), Some("pl.session.advance.decompose"), "PL deciding split → decompose", false) },
    Rule { name: "pl-tuning", out: RuleOut::Obj(Some("pl"), Some("pl.session.finalize"), "PL tuning → finalize", false) },
    Rule { name: "pl-awaiting-approval", out: RuleOut::Obj(Some("pl"), Some("pl.session.approve"), "PL awaiting approval → auto-approve (driver)", false) },
    Rule { name: "pl-awaiting-approval-hold", out: RuleOut::Obj(None, None, "awaiting human approval (ANTILOKI_AUTO_APPROVE=off) — a human must dispatch pl.session.approve", false) },
    Rule { name: "pl-done-no-contract", out: RuleOut::Obj(Some("tl"), Some("contract.draft.auto"), "PL session done → TL drafts contract", false) },
    Rule { name: "contract-rejected-escalate-after-too-many", out: RuleOut::Obj(None, None, "escalate-to-human: attempt cap 2 reached (cardRejectionCount OR autoRedraftsSinceManual OR manBlockedRedrafts) — auto-redraft budget exhausted, human inspector required", true) },
    Rule { name: "contract-rejected-redo-operator-restart", out: RuleOut::Obj(Some("tl"), Some("tl.contract.redraft"), "operator restart → TL re-draft (cap bypassed; explicit human signal)", false) },
    Rule { name: "contract-rejected-redo", out: RuleOut::Obj(Some("tl"), Some("tl.contract.redraft"), "rejected contract → TL re-draft (MAN will audit revision via standard Phase 1 gate)", false) },
    Rule { name: "contract-proposed", out: RuleOut::Obj(Some("pl"), Some("card.gate.pl.ratify"), "contract proposed → PL ratifies", false) },
    Rule { name: "swarm-needs-manifest-draft", out: RuleOut::Obj(Some("tl"), Some("swarm.manifest.design"), "swarm on + no manifest yet → TL designs the blueprint nucleus (the typed dataflow manifest)", false) },
    Rule { name: "swarm-needs-manifest-lint", out: RuleOut::Obj(Some("qa"), Some("swarm.manifest.lint"), "manifest drafted → deterministic lint gate (structure + bindings + type agreement)", false) },
    Rule { name: "swarm-manifest-lint-failed-redesign", out: RuleOut::Obj(Some("tl"), Some("swarm.manifest.design"), "manifest lint RED → TL re-designs the manifest (bounded)", false) },
    Rule { name: "swarm-needs-author", out: RuleOut::Obj(Some("tl"), Some("swarm.author.blocks"), "manifest GREEN → LOW author swarm writes each block + the composition gate runs", false) },
    Rule { name: "swarm-composition-failed-reauthor", out: RuleOut::Obj(Some("tl"), Some("swarm.author.blocks"), "composition RED → re-author the blocks that do not compose (bounded)", false) },
    Rule { name: "swarm-needs-review", out: RuleOut::Obj(Some("qa"), Some("swarm.review.contract"), "blocks composed → adversarial review swarm judges each file (advisory)", false) },
    Rule { name: "swarm-needs-coverage", out: RuleOut::Obj(Some("qa"), Some("qa.swarm.coverage"), "blocks authored + reviewed → deterministic grep-acceptance coverage gate (against authored content)", false) },
    Rule { name: "swarm-coverage-failed-reauthor", out: RuleOut::Obj(Some("tl"), Some("swarm.author.blocks"), "swarm-coverage RED → re-author the files missing the acceptance literal (bounded)", false) },
    Rule { name: "swarm-stuck-escalate", out: RuleOut::Obj(None, None, "swarm exhausted its design/author budget without a clean blueprint — escalate to a human", true) },
    Rule { name: "contract-pl-ratified-needs-plan", out: RuleOut::Obj(Some("tl"), Some("tl.plan.contract"), "size M/L needs a microtask plan (defs) before authoring/signing", false) },
    Rule { name: "contract-planned-needs-plan-coverage", out: RuleOut::Obj(Some("qa"), Some("qa.contract.plan.coverage"), "plan defs ready → deterministic coverage gate (file-exists + grep) runs first, before the AI review", false) },
    Rule { name: "contract-plan-coverage-failed-retry", out: RuleOut::Obj(Some("tl"), Some("tl.plan.contract"), "plan-coverage RED → TL re-plans against the exact uncovered acceptances (deterministic — converges in one round)", false) },
    Rule { name: "contract-planned-needs-plan-review", out: RuleOut::Obj(Some("qa"), Some("qa.contract.review"), "plan-coverage GREEN → QA reviews the decomposition (mode=plan, advisory) for open-ended quality", false) },
    Rule { name: "contract-planned-needs-snippets", out: RuleOut::Obj(Some("tl"), Some("tl.author.snippets"), "plan-coverage GREEN + AI review done (microtask findings advisory) → TL authors the per-file snippets", false) },
    Rule { name: "contract-pl-ratified", out: RuleOut::Obj(Some("tl"), Some("contract.gate.tl.sign"), "PL ratified → TL signs", false) },
    Rule { name: "contract-tl-approved-high-risk", out: RuleOut::Obj(None, None, "high-risk contract awaiting human approval", false) },
    Rule { name: "contract-tl-approved-needs-pl-review", out: RuleOut::Obj(Some("pl"), Some("pl.review.contract"), "TL approved → PL reviews the contract (handshake before MAN Phase 1)", false) },
    Rule { name: "contract-tl-approved-needs-qa-review", out: RuleOut::Obj(Some("qa"), Some("qa.contract.review"), "PL signed → QA #1 reviews the contract + microtasks + literal patches before DEV", false) },
    Rule { name: "contract-tl-approved-qa-review-contract-level-escalate", out: RuleOut::Obj(None, None, "escalate-to-human: QA #1 found a CONTRACT-level defect (acceptance/scope/goal) that microtask re-authoring cannot fix — QA's exact fix hints are on the escalation notification", true) },
    Rule { name: "contract-tl-approved-qa-review-failed-retry", out: RuleOut::Obj(Some("tl"), Some("tl.author.snippets"), "QA #1 RED → TL re-authors the snippets against QA findings before DEV (no re-plan)", false) },
    Rule { name: "contract-tl-approved-qa-review-failed-escalate", out: RuleOut::Obj(None, None, "escalate-to-human: QA #1 contract review still RED after 3 re-authors — findings are in the latest contract_qa_reviews.findings_md", true) },
    Rule { name: "contract-tl-approved-needs-man-phase1", out: RuleOut::Obj(Some("man"), Some("man.audit.contract.signed"), "PL signed + QA #1 GREEN (or swarm authored + reviewed) → MAN Phase 1 audit before sandbox/DEV", false) },
    Rule { name: "contract-tl-approved-needs-worktree", out: RuleOut::Obj(Some("git-manager"), Some("git-manager.worktree.create"), "ANTILOKI_WORKTREE on → create a git worktree build env (full checkout) before DEV", false) },
    Rule { name: "contract-tl-approved-no-sandbox", out: RuleOut::Obj(Some("tl"), Some("tl.sandbox.create"), "TL approved + MAN Phase 1 passed → create sandbox before DEV", false) },
    Rule { name: "contract-tl-approved", out: RuleOut::Obj(Some("git-manager"), Some("git-manager.implement.code"), "TL approved + MAN Phase 1 passed + build env ready (sandbox or worktree) → DEV implements", false) },
    Rule { name: "contract-human-approved", out: RuleOut::Obj(Some("git-manager"), Some("git-manager.implement.code"), "human approved → DEV implements", false) },
    Rule { name: "contract-impl-needs-build-gate", out: RuleOut::Obj(Some("git-manager"), Some("git-manager.build.gate"), "DEV applied (status=impl) → run the deterministic build gate before QA", false) },
    Rule { name: "contract-impl-build-gate-failed-retry", out: RuleOut::Obj(Some("tl"), Some("tl.author.snippets"), "build gate RED → TL re-authors snippets against the real compiler errors before QA runs (no re-plan)", false) },
    Rule { name: "contract-impl-build-gate-failed-escalate", out: RuleOut::Obj(None, None, "escalate-to-human: build gate still RED after 3 re-authors — the real compiler error is in the latest build_gate_runs.evidence_md", true) },
    Rule { name: "contract-impl-no-qa", out: RuleOut::Obj(Some("qa"), Some("qa.gate.contract.verify"), "DEV applied + build gate GREEN → QA verifies", false) },
    Rule { name: "contract-impl-qa-escalation-reason-set", out: RuleOut::Obj(None, None, "escalate-to-human: QA set escalation_reason (e.g. tooling failure outside contract scope)", true) },
    Rule { name: "contract-impl-qa-failed-no-more-work-escalate", out: RuleOut::Obj(None, None, "escalate-to-human: QA failed AND DEV finished every microtask AND no fresh QA suggestions to consume — no remediation work left", true) },
    Rule { name: "contract-impl-qa-failed-amendment-to-tl", out: RuleOut::Obj(Some("tl"), Some("tl.contract.amend"), "QA emitted contract-amendment microtask → TL applies scope change before DEV retries", false) },
    Rule { name: "contract-impl-amendment-cap-reached-escalate", out: RuleOut::Obj(None, None, "escalate-to-human: TL_MAX_AMENDMENTS=2 reached — contract has needed scope changes too many times", true) },
    Rule { name: "contract-impl-qa-failed-retry", out: RuleOut::Obj(Some("tl"), Some("tl.author.snippets"), "QA failed → TL re-authors snippets (DEV is deterministic; a DEV replay would repeat the failure; no re-plan)", false) },
    Rule { name: "contract-impl-qa-failed-escalate", out: RuleOut::Obj(None, None, "escalate-to-human: 3 retries exhausted (dev re-applies OR TL re-authors)", true) },
    Rule { name: "contract-qa", out: RuleOut::Obj(Some("qa"), Some("qa.gate.contract.verify"), "contract at QA gate", false) },
    Rule { name: "contract-done-needs-pr", out: RuleOut::Obj(Some("git-manager"), Some("git-manager.pr.open"), "final QA green → open the card's pull request for operator review (auto-merge is off)", false) },
    Rule { name: "contract-done-pr-pending", out: RuleOut::Obj(None, None, "pull request open — merge it from the Pulls screen (or enable auto-merge on the card)", false) },
    Rule { name: "contract-done-needs-worktree-merge", out: RuleOut::Obj(Some("git-manager"), Some("git-manager.worktree.merge"), "QA green + worktree build env + auto-merge on → merge worktree back to base before closing", false) },
    Rule { name: "contract-done-needs-man-review", out: RuleOut::Obj(Some("man"), Some("man.card.review"), "contract done → MAN audits the journey", false) },
    Rule { name: "contract-done-needs-keeper-consolidate", out: RuleOut::Obj(Some("keeper"), Some("keeper.card.consolidate"), "MAN reviewed → Keeper distills lessons", false) },
    Rule { name: "contract-done-needs-journey-append", out: RuleOut::Obj(Some("keeper"), Some("keeper.journey.append"), "lesson consolidated → append to project_journey", false) },
    Rule { name: "contract-done-needs-review-blueprint", out: RuleOut::Obj(Some("board-keeper"), Some("board-keeper.card.review.blueprint.generate"), "journey appended → freeze the review blueprint", false) },
    Rule { name: "contract-done-needs-atlas-integrate", out: RuleOut::Obj(Some("keeper"), Some("system.codebase.integrate"), "post-QA reflections done → fold the built code into the atlas", false) },
    Rule { name: "contract-done-card-needs-close", out: RuleOut::Obj(Some("board-keeper"), Some("board-keeper.card.status.change"), "post-QA chain complete (MAN + keeper + journey + blueprint) → close the card", false) },
    Rule { name: "contract-done", out: RuleOut::Obj(None, None, "contract done — card closes", true) },
    Rule { name: "contract-rejected-or-superseded", out: RuleOut::Obj(None, None, "contract rejected or superseded", true) },
];

fn tag_for(r: &Rule) -> &'static str {
    match &r.out {
        RuleOut::FnValued => r#"<span class="tag work">work</span>"#,
        RuleOut::Obj(_, next_action, _, terminal) => {
            if *terminal {
                r#"<span class="tag term">terminal</span>"#
            } else if next_action.is_none() {
                r#"<span class="tag pause">pause</span>"#
            } else {
                r#"<span class="tag work">work</span>"#
            }
        }
    }
}

fn render_rules_table_lines() -> Vec<String> {
    let mut lines: Vec<String> = vec![
        String::new(),
        "    <table>".to_string(),
        r#"      <thead><tr><th>#</th><th>rule name</th><th>kind</th><th>next agent</th><th>next action</th><th>reason</th></tr></thead>"#.to_string(),
        "      <tbody>".to_string(),
        "        ".to_string(),
    ];
    for (i, r) in RULES.iter().enumerate() {
        let (agent_cell, action_cell, reason_cell): (String, String, String) = match &r.out {
            RuleOut::FnValued => (
                r#"<span class="muted">—</span>"#.to_string(),
                r#"<span class="muted">—</span>"#.to_string(),
                String::new(),
            ),
            RuleOut::Obj(next_agent, next_action, reason, _) => (
                match next_agent {
                    Some(a) => format!("<code>{}</code>", esc_html(a)),
                    None => r#"<span class="muted">—</span>"#.to_string(),
                },
                match next_action {
                    Some(a) => format!("<code>{}</code>", esc_html(a)),
                    None => r#"<span class="muted">—</span>"#.to_string(),
                },
                esc_html(reason),
            ),
        };
        lines.push("          <tr>".to_string());
        lines.push(format!(r#"            <td class="muted">{}</td>"#, i + 1));
        lines.push(format!("            <td><code>{}</code></td>", esc_html(r.name)));
        lines.push(format!("            <td>{}</td>", tag_for(r)));
        lines.push(format!("            <td>{}</td>", agent_cell));
        lines.push(format!("            <td>{}</td>", action_cell));
        lines.push(format!("            <td>{}</td>", reason_cell));
        lines.push("          </tr>".to_string());
    }
    lines.push("      </tbody>".to_string());
    lines.push("    </table>".to_string());
    lines.push(r#"    <p class="meta">Rules are evaluated in declared order — first match wins. <code>terminal</code> = card is done from the driver's perspective. <code>pause</code> = non-terminal but driver can't act (e.g., awaiting human). <code>work</code> = driver will dispatch this on the next tick.</p>"#.to_string());
    lines.push("  ".to_string());
    lines
}

pub fn driverDocHtml(_input: &Value, _db: &Connection) -> Result<Value, String> {
    let mut lines: Vec<String> = vec![
        "<!DOCTYPE html>".to_string(),
        [r#"<html><head><meta charset="UTF-8"><title>antiloki · driver</title><style>"#, CSS, r#"</style></head>"#].concat(),
        ["<body>", NAV, r#"<div class="wrap">"#].concat(),
        r#"<div class="kicker">antiloki-v3 · driver</div>"#.to_string(),
        "<h1>Driver — orchestration loop</h1>".to_string(),
        r#"<p class="lede">The driver is the substrate's transmission: a deterministic loop that reads card+contract+qa state and dispatches the next correct action against the next correct agent, until quiescent or budget-exhausted.</p>"#.to_string(),
        [r#"<p class="meta">Generated <code>"#, &generated_at(), r#"</code> · regen <code>node bin/antiloki.mjs system.docs.driver '{}' &gt; docs/driver.html</code></p>"#].concat(),
        String::new(),
        "<h2>What the driver is</h2>".to_string(),
        "<ul>".to_string(),
        r#"  <li><strong>Not an agent.</strong> The driver directs the 10 agents but is not one of them. It identifies in <code>sacred_timeline</code> as <code>actor='driver:auto'</code>.</li>"#.to_string(),
        r#"  <li><strong>Deterministic.</strong> The state machine is a pure data table — no LLM-routed decisions. The same (card, contract, qa) state always picks the same next action.</li>"#.to_string(),
        r#"  <li><strong>Single-threaded.</strong> One tick at a time, one card at a time. Concurrency is a future concern.</li>"#.to_string(),
        r#"  <li><strong>Audit-anchored.</strong> Every tick is recorded in <code>brain.driver_ticks</code> with a forward link (<code>fired_event_id</code>) into <code>sacred_timeline</code>.</li>"#.to_string(),
        "</ul>".to_string(),
        String::new(),
        "<h2>The state machine</h2>".to_string(),
        [r#"<p>"#, &RULES.len().to_string(), r#" rules, evaluated top-down. The first rule whose <code>match(facts)</code> predicate returns true wins. Facts gathered by <code>driver-state-machine.mjs</code>: <code>{card, contract, plSession, lastQaRun, devAttempts}</code>.</p>"#].concat(),
    ];
    lines.extend(render_rules_table_lines());
    lines.push([r#"<p class="muted">Retry budget for failed QA: <code>MAX_DEV_RETRIES = "#, &MAX_DEV_RETRIES.to_string(), r#"</code> — after that many <code>dev.implement.code</code> attempts since the last failing <code>qa_run</code>, the rule <code>contract-impl-qa-failed-escalate</code> fires and the card is moved to <code>blocked</code> with a <code>correction</code> memory recording the failure.</p>"#].concat());
    lines.push(String::new());
    lines.push("<h2>QA-fail retries — sliced vs full</h2>".to_string());
    lines.push(r#"<p>The QA-fail retry loop is <strong>closed and sliced</strong> (FEAT-SLICED-RETRIES, 2026-06-10). On a failing QA run the driver dispatches <code>tl.refine.microtasks</code>, which decides <em>full vs sliced</em> re-authoring <strong>deterministically, before any AI call</strong>:</p>"#.to_string());
    lines.push("<ul>".to_string());
    lines.push(r#"  <li><code>implicateMicrotasks</code> maps each failing acceptance criterion onto microtasks by file overlap — failing ACs implicate the microtasks that touch the same files.</li>"#.to_string());
    lines.push(r#"  <li><code>decideRetryMode</code> applies conservative rules: <strong>full</strong> re-author when the build gate is red, there is no per-AC signal, most ACs are failing, the failing ACs are unmappable to files, a majority of microtasks is implicated, the acceptance list changed since the run, or a previous <em>sliced</em> attempt already failed on the same (or a superset of the) AC set. Only a clear minority-implicated signal yields <strong>sliced</strong>.</li>"#.to_string());
    lines.push("</ul>".to_string());
    lines.push(r#"<p>In <strong>sliced</strong> mode the passing microtasks are kept <strong>immutable</strong> (verbatim, original order) and only the implicated ones are re-authored; replacements carry a per-microtask <code>origin</code> stamp, and merge guards escalate to full if the AI's replacement set grows into kept ids or comes back empty.</p>"#.to_string());
    lines.push(r#"<div class="panel good">"#.to_string());
    lines.push(r#"  <strong>The loop actually re-dispatches DEV now.</strong> After ANY re-author the contract flips back to <code>status='tl_approved'</code> with the plan cursor (<code>dev_microtask_progress</code>) pointed past the kept, already-applied microtasks — so the new patches go back through the QA contract-review gate and DEV re-applies them. Before this fix the contract sat at <code>status='impl'</code> with a stale cursor and the loop was refine→refine→refine→escalate without DEV ever running again (FIX-QA-RETRY-LOOP-NEVER-REDISPATCHES-DEV)."#.to_string());
    lines.push("</div>".to_string());
    lines.push([r#"<p class="muted">Provenance: every retry decision (mode, why, attempt, qa_run_id, kept/replaced ids) is appended to <code>contracts.refine_history_json</code>; the retry caps are unchanged (<code>MAX_DEV_RETRIES = "#, &MAX_DEV_RETRIES.to_string(), r#"</code> for DEV re-applies and TL re-authors alike). Source: <code>.antiloki/skills/agents/tl/tl-refine-microtasks.mjs</code> (<code>implicateMicrotasks</code> · <code>decideRetryMode</code> · <code>mergeSlicedPlan</code>).</p>"#].concat());
    lines.push(String::new());
    lines.push("<h2>The tick lifecycle</h2>".to_string());
    lines.push("<ol>".to_string());
    lines.push(r#"  <li>INSERT <code>brain.driver_ticks</code> row with <code>outcome='running'</code></li>"#.to_string());
    lines.push(r#"  <li>Halt-file check — if <code>.antiloki/run/driver-halt</code> exists, finalize <code>halt_requested</code> and return</li>"#.to_string());
    lines.push(r#"  <li><code>driver-schedule</code> picks the next actionable card (or returns null on empty board)</li>"#.to_string());
    lines.push(r#"  <li>Escalation pass — if any candidate hit the escalate-to-human rule, mark it blocked + write a correction memory, finalize <code>escalated</code></li>"#.to_string());
    lines.push(r#"  <li>If no chosen card, finalize <code>idle</code> with reason capturing whether board was empty vs. all paused</li>"#.to_string());
    lines.push(r#"  <li>Dispatch via <code>action-run({action: decision.next_action, input: {card_slug, slug}}, {db, actor: 'driver:auto'})</code></li>"#.to_string());
    lines.push(r#"  <li>Capture <code>fired_event_id</code> (latest <code>sacred_timeline.id</code> since <code>started_at</code>) and <code>cost_usd</code> (SUM of <code>brain.ai_calls.cost_usd_actual</code> since <code>started_at</code>)</li>"#.to_string());
    lines.push(r#"  <li>UPDATE the tick row with terminal outcome, duration, ended_at, decided_*, error_message (if any)</li>"#.to_string());
    lines.push("</ol>".to_string());
    lines.push(String::new());
    lines.push("<h2>Budget caps (driver-run)</h2>".to_string());
    lines.push("<table>".to_string());
    lines.push(r#"  <thead><tr><th>knob</th><th>default</th><th>what halts the loop</th></tr></thead>"#.to_string());
    lines.push("  <tbody>".to_string());
    lines.push(r#"    <tr><td><code>max_ticks</code></td>      <td>50</td>   <td>after N iterations</td></tr>"#.to_string());
    lines.push(r#"    <tr><td><code>max_cost_usd</code></td>   <td>1.0</td>  <td>when <code>SUM(driver_ticks.cost_usd) WHERE started_at &gt;= runStart</code> reaches the cap (DB-anchored — synthetic rows count too)</td></tr>"#.to_string());
    lines.push(r#"    <tr><td><code>max_wall_s</code></td>     <td>300</td>  <td>after N seconds of real time</td></tr>"#.to_string());
    lines.push(r#"    <tr><td><code>stop_on_idle</code></td>   <td>true</td> <td>first <code>idle</code> tick — stop instead of polling forever</td></tr>"#.to_string());
    lines.push("  </tbody>".to_string());
    lines.push("</table>".to_string());
    lines.push(String::new());
    lines.push("<h2>Escape hatches</h2>".to_string());
    lines.push(r#"<div class="panel warn">"#.to_string());
    lines.push("  <h3>Halt the running driver</h3>".to_string());
    lines.push("  <pre><code>touch .antiloki/run/driver-halt".to_string());
    lines.push("# next tick's first action is to detect this file and exit cleanly".to_string());
    lines.push("# (outcome='halt_requested' / halt_reason='halt_file')".to_string());
    lines.push("rm .antiloki/run/driver-halt   # resume next time you start the loop</code></pre>".to_string());
    lines.push("</div>".to_string());
    lines.push(r#"<div class="panel">"#.to_string());
    lines.push("  <h3>Run as god (no permission checks)</h3>".to_string());
    lines.push("  <pre><code>node bin/antiloki.mjs --as god system.driver.run '{}'</code></pre>".to_string());
    lines.push(r#"  <p class="muted">Requires interactive TTY (or <code>ANTILOKI_GOD_AUTHORIZED=true</code>) — AI subprocesses without PTY are blocked from god mode.</p>"#.to_string());
    lines.push("</div>".to_string());
    lines.push(r#"<div class="panel">"#.to_string());
    lines.push("  <h3>Bypass audit-vs-code drift law during a run</h3>".to_string());
    lines.push("  <pre><code>ANTILOKI_ALLOW_DRIFT=true node bin/antiloki.mjs system.driver.run '{}'</code></pre>".to_string());
    lines.push(r#"  <p class="muted">Disables the <code>COMMIT_CODE_BEFORE_AUDIT_COMMIT</code> law. Use sparingly; commits stop being legible.</p>"#.to_string());
    lines.push("</div>".to_string());
    lines.push(String::new());
    lines.push("<h2>Inspect what the driver did</h2>".to_string());
    lines.push("<pre><code># last 20 ticks at a glance".to_string());
    lines.push("node --experimental-sqlite --disable-warning=ExperimentalWarning -e \"".to_string());
    lines.push(r#"const db = new (require('node:sqlite').DatabaseSync)('./.antiloki/db/state.sqlite', {readOnly:true});"#.to_string());
    lines.push(r#"db.exec(\`ATTACH DATABASE './.antiloki/db/brain.sqlite' AS brain\`);"#.to_string());
    lines.push(r#"console.table(db.prepare(\`SELECT id, card_slug, decided_action, outcome, duration_ms, cost_usd FROM brain.driver_ticks ORDER BY id DESC LIMIT 20\`).all());"#.to_string());
    lines.push("\"".to_string());
    lines.push(String::new());
    lines.push("# or in the TUI:".to_string());
    lines.push("npm run tui".to_string());
    lines.push("# then press 'd' to toggle the driver pane (left/slot-3)</code></pre>".to_string());
    lines.push(String::new());
    lines.push("<hr>".to_string());
    lines.push(r#"<p class="muted">Source of truth for this page: <code>.antiloki/skills/driver/state-machine-rules.mjs</code> + <code>.antiloki/skills/driver/driver-tick.mjs</code> + <code>.antiloki/skills/driver/driver-run.mjs</code>. When the rule table or tick lifecycle changes, regenerate this page so it stays in sync.</p>"#.to_string());
    lines.push("</div></body></html>".to_string());
    Ok(Value::String(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn out() -> String {
        match driverDocHtml(&json!({}), &Connection::open_in_memory().unwrap()).unwrap() {
            Value::String(s) => s,
            _ => unreachable!(),
        }
    }
    #[test]
    fn byte_exact_structure() {
        let s = out();
        assert!(s.starts_with("<!DOCTYPE html>\n<html><head><meta charset=\"UTF-8\"><title>antiloki · driver</title><style>\n:root {"));
        assert!(s.ends_with("</div></body></html>"));
        assert!(!s.contains('\r'), "output must be LF-only");
        assert!(s.contains("← antiloki docs</a></nav>"));
        assert!(s.contains("<h1>Driver — orchestration loop</h1>"));
        assert!(s.contains(&format!("<p>{} rules, evaluated top-down.", RULES.len())));
        assert_eq!(RULES.len(), 69);
    }
    #[test]
    fn lists_every_rule_name() {
        let s = out();
        for r in RULES {
            assert!(s.contains(&format!("<code>{}</code>", r.name)), "rule '{}' missing from doc", r.name);
        }
    }
    #[test]
    fn fn_valued_out_rule_degrades_to_dashes() {
        let s = out();
        // repeat-failure-escalate: JS `out` is a function; property reads are all `undefined`.
        assert!(s.contains(r#"<td><code>repeat-failure-escalate</code></td>
            <td><span class="tag work">work</span></td>
            <td><span class="muted">—</span></td>
            <td><span class="muted">—</span></td>
            <td></td>"#));
    }
    #[test]
    fn documents_budget_caps_and_halt_file() {
        let s = out();
        assert!(s.contains("max_ticks"));
        assert!(s.contains("max_cost_usd"));
        assert!(s.contains("max_wall_s"));
        assert!(s.contains("driver-halt"));
    }
    #[test]
    fn escapes_angle_brackets_in_reason_text() {
        let s = out();
        assert!(s.contains("type `do &lt;slug&gt;` in chat to start"));
    }
    #[test]
    fn deterministic_and_ignores_input() {
        let a = driverDocHtml(&json!({}), &Connection::open_in_memory().unwrap()).unwrap();
        let b = driverDocHtml(&json!({"foo": "bar"}), &Connection::open_in_memory().unwrap()).unwrap();
        assert_eq!(a, b);
    }
}
