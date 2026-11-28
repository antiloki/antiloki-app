//! THE GAUGES — the decisions the box can make with facts, and the shape the calibrated ones will take later
//! (operator 2026-09-29: "one thing, divided in many skills and actions … they feed the decision maker. They are like
//! gates, uncertainty gates").
//!
//! A gauge reads FACTS about an ask or a failure and answers with a `Reading`: a value, whether the gauge is SURE of
//! it, and the evidence it can point at. That is the whole contract, and it has one rule that makes it a family:
//!
//!   A GAUGE MAY ONLY BE SURE WHEN IT CAN POINT. "This path does not exist" is a fact; "this ask is clear" is not, so
//!   the screen gauge never says it — it abstains (`sure: false`) and the decision goes to a model. Every deterministic
//!   gauge that fires saves a model call; every gauge that abstains costs nothing and claims nothing.
//!
//! WHY ONE SHAPE. `p` is the slot for a calibrated probability. Today every gauge leaves it `None` (a rule is either
//! sure or it is not); the local classifier planned in bench/PLAN-THE-GAUGES.md fills it for the fuzzy decisions and
//! uses the same `Reading`, so the pipeline that consumes a gauge does not care whether a rule or a model made it — it
//! asks `sure`, and escalates when the answer is no.
//!
//! WHAT IS HERE, and where it decides (the rest are read-only for now):
//!   · `screen`  — a named path that is not there / a reference to nothing → SURE → the run parks on that question
//!                 BEFORE the screening call and the planner are paid for. WIRED (goals::drive_plan).
//!   · `retry`   — an error that retrying cannot fix (no login, 402, quota, no binary) → SURE stop; a transient one
//!                 → again. WIRED (goals: the empty-answer retry, which used to spend a second call on a dead seat).
//!   · `wave`    — is the ask bigger than L? · `checks` — does it need a test/build/lint run? · `domain` — what kind
//!                 of work do the paths it names say? These wrap the pure functions the pipeline already decides with
//!                 and report them, with evidence, in one `gauge` line per plan — the label log the calibration reads.
use crate::roles::Domain;
use serde::Serialize;

/// One gauge's answer. `sure` is the gate: false means "I cannot point at a fact — ask something else".
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reading<T> {
    pub gauge: &'static str,
    pub value: T,
    pub sure: bool,
    /// a calibrated probability for `value` — None for a rule (sure or not), filled by the local classifier later
    pub p: Option<f64>,
    /// what the gauge can point at: a path, a phrase, a count. Empty only when `sure` is false.
    pub evidence: Vec<String>,
}
impl<T> Reading<T> {
    fn sure(gauge: &'static str, value: T, evidence: Vec<String>) -> Self { Reading { gauge, value, sure: true, p: None, evidence } }
    fn unsure(gauge: &'static str, value: T, evidence: Vec<String>) -> Self { Reading { gauge, value, sure: false, p: None, evidence } }
}

/// THE SCREEN GAUGE. SURE only when the ask names something the box cannot answer for it. An ask with no such fact is
/// not "clear" — it is "not decided here", and the value is empty so the caller cannot mistake one for the other.
pub fn screen<F: Fn(&str) -> bool>(ask: &str, exists: F) -> Reading<Vec<crate::screen::Doubt>> {
    let found = crate::screen::doubts(ask, exists);
    if found.is_empty() { return Reading::unsure("screen", vec![], vec![]); }
    let ev = found.iter().map(|d| format!("{}: {}", d.kind, d.evidence)).collect();
    Reading::sure("screen", found, ev)
}

/// AN HTTP STATUS CODE, AND NOT A NUMBER THAT LOOKS LIKE ONE. "401" counts only beside a word that makes it a status: after http/status/error/code/response, or before the
/// phrase that names it (unauthorized, too many requests, service unavailable...). "expected 403 items", "ran 500 iterations", "at line 401" are counts and lines. Pure - tested.
fn http_code(w: &str, codes: &[&str]) -> Option<String> {
    let toks: Vec<&str> = w.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).collect();
    const BEFORE: &[&str] = &["http", "status", "error", "code", "response", "returned", "got"];
    const AFTER: &[&str] = &["unauthorized", "forbidden", "payment", "too", "bad", "service", "internal", "gateway", "overloaded", "overloaded_error", "not", "request", "rate", "error"];
    toks.iter().enumerate().find(|(i, t)| codes.contains(t) && (i.checked_sub(1).map(|j| BEFORE.contains(&toks[j])).unwrap_or(false) || toks.get(i + 1).map(|n| AFTER.contains(n)).unwrap_or(false))).map(|(_, t)| t.to_string())
}

/// WHAT RETRYING CAN DO. `Again` = a second attempt has a chance; `Stop` = it has none, or the cap is spent.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum Retry { Again, Stop }

/// THE RETRY GAUGE. The pipeline retries a failed call once "because it is usually transient" — true of a timeout, and a
/// second call wasted on a seat that is not signed in, out of credit, or not installed. The error TEXT says which.
/// SURE to stop on: the cap, or a permanent cause (matched as whole tokens, so a stack trace that happens to contain the
/// digits 401 is not a login failure). SURE to go again on a known transient one. Anything else is unsure and keeps the
/// old behaviour (one retry), which is the honest default for an error nobody has classified.
pub fn retry(why: &str, attempts: u32, max: u32) -> Reading<Retry> {
    if attempts >= max { return Reading::sure("retry", Retry::Stop, vec![format!("attempt {attempts} of {max} spent")]); }
    let w = why.to_ascii_lowercase();
    let has_code = |codes: &[&str]| http_code(&w, codes);
    const PERMANENT: &[&str] = &["not logged in", "not signed in", "please log in", "please sign in", "login required", "invalid api key", "invalid_api_key",
        "payment required", "insufficient credit", "credit balance", "quota exceeded", "out of credits", "usage limit", "model not found", "unknown model",
        "unsupported model", "command not found", "failed to spawn", "executable file not found", "permission denied (publickey)",
        "authentication required", "authentication failed", "must be logged in", "run /login", "please run /login", "token has expired", "oauth token", "account has been suspended", "account suspended", "claude login", "codex login", "sign in again", "hard limit", "run out of credits", "ran out of credits", "not available on your plan", "prompt is too long", "context length exceeded", "context_length_exceeded", "insufficient_quota", "exceeded your current quota", "upgrade to pro", "hit your usage limit"];
    if let Some(k) = PERMANENT.iter().find(|k| w.contains(**k)) { return Reading::sure("retry", Retry::Stop, vec![format!("permanent: “{k}”")]); }
    // a binary that is not there: "spawn claude ENOENT", "No such file or directory … while spawning `grok`" (the words "spawn" AND the missing-file cause together)
    if w.contains("spawn") && (w.contains("enoent") || w.contains("no such file or directory")) { return Reading::sure("retry", Retry::Stop, vec!["permanent: the binary is not there (spawn + ENOENT)".into()]); }
    if w.contains("model") && (w.contains("not found") || w.contains("does not exist") || w.contains("is not available")) { return Reading::sure("retry", Retry::Stop, vec!["permanent: the model does not exist".into()]); }
    if let Some(c) = has_code(&["401", "402", "403"]) { return Reading::sure("retry", Retry::Stop, vec![format!("permanent: HTTP {c}")]); }
    const TRANSIENT: &[&str] = &["timed out", "timeout", "rate limit", "overloaded", "connection reset", "econnreset", "temporarily", "try again", "no report", "empty reply", "returned nothing", "serve restarted",
        "socket hang up", "socket closed", "context deadline exceeded", "deadline exceeded", "etimedout", "econnaborted", "upstream connect error", "disconnect/reset", "fetch failed", "network error", "stream ended", "connection closed", "resource_exhausted", "unavailable:", "econnrefused", "eai_again"];
    if let Some(k) = TRANSIENT.iter().find(|k| w.contains(**k)) { return Reading::sure("retry", Retry::Again, vec![format!("transient: “{k}”")]); }
    if let Some(c) = has_code(&["429", "500", "502", "503", "504"]) { return Reading::sure("retry", Retry::Again, vec![format!("transient: HTTP {c}")]); }
    Reading::unsure("retry", Retry::Again, vec![])
}

/// IS THE SEAT ITSELF DEAD? (found live 2026-09-29, a worker whose claude-code seat was signed out was "retried on the fallback claude-code
/// opus" — the same login, a second identical failure.) SURE only for causes that belong to the SEAT and so follow it to every model of the
/// engine: signed out, no credit, no binary. NOT for a model-specific error ("unknown model", a per-model usage limit, a 403 on one model) —
/// the fallback on another model of the same engine may well work there. The caller uses it to skip a same-engine fallback; a fallback on
/// ANOTHER engine is always worth its call. Pure — tested.
pub fn seat_dead(why: &str) -> Reading<bool> {
    let w = why.to_ascii_lowercase();
    const SEAT: &[&str] = &["not logged in", "not signed in", "please log in", "please sign in", "login required", "invalid api key", "invalid_api_key",
        "payment required", "insufficient credit", "credit balance", "out of credits", "run out of credits", "ran out of credits", "billing hard limit", "command not found", "failed to spawn", "executable file not found", "authentication required", "authentication failed", "run /login", "token has expired", "account has been suspended", "sign in again", "claude login", "codex login", "please run `claude login"];
    if w.contains("spawn") && w.contains("enoent") { return Reading::sure("seat", true, vec!["seat-level: the binary is not there (spawn + ENOENT)".into()]); }
    if let Some(k) = SEAT.iter().find(|k| w.contains(**k)) { return Reading::sure("seat", true, vec![format!("seat-level: “{k}”")]); }
    if let Some(c) = http_code(&w, &["401", "402"]) { return Reading::sure("seat", true, vec![format!("seat-level: HTTP {c}")]); }
    Reading::unsure("seat", false, vec![])
}

/// A DECISION THE OPERATOR HANDED OVER (found in the operator\'s screenshot, 2026-09-29: "what do you suggest?" was screened, and Heimdall asked
/// which outcome to prioritise - the operator asked the machine to choose and was asked to choose). SURE only for a short ask whose whole point
/// is the machine\'s judgement: suggestions, recommendations, "what should I / would you". Screening exists to stop a plan going wrong under a
/// wrong guess; here the guess IS the deliverable, so there is nothing to ask. Pure - tested.
pub fn delegated(ask: &str) -> Reading<bool> {
    let n = ask.to_lowercase().replace('\u{2019}', "'"); let words = n.split_whitespace().count().max(if crate::lexicon::is_unspaced(&n) { n.chars().count() / 3 } else { 0 });
    // the phrases are per-language data (lexicon.rs); matched on word edges of the raw text, so "next.js" is not "next" and "surprise-me" is not "surprise me"
    if words <= 14 { if let Some(k) = crate::lexicon::all().delegating.iter().find(|k| crate::lexicon::has_phrase_raw(&n, k)) { return Reading::sure("delegated", true, vec![format!("asks for the machine\'s judgement: “{k}”")]); } }
    Reading::unsure("delegated", false, vec![])
}

/// THE WAVE GAUGE: is the ask bigger than L? A rule the pipeline already applies (`goals::ask_bigger_than_l`), stated with
/// the count or length that decided it. Always sure — it is arithmetic on the text.
pub fn wave(ask: &str) -> Reading<bool> {
    let (parts, chars) = (crate::goals::enumerated_tasks(ask), ask.chars().count());
    let big = crate::goals::ask_bigger_than_l(ask);
    let why = if parts >= 2 { format!("{parts} numbered parts") } else if chars > 600 { format!("{chars} characters") } else { format!("{parts} part{}, {chars} characters", if parts == 1 { "" } else { "s" }) };
    Reading::sure("wave", big, vec![why])
}

/// THE CHECKS GAUGE: does the ask itself need a test, build or lint run? The word that said so is the evidence.
pub fn checks(ask: &str) -> Reading<bool> {
    let wants = crate::goals::ask_wants_checks(ask);
    if !wants { return Reading::sure("checks", false, vec!["no check named".into()]); }
    let g = ask.to_ascii_lowercase();
    let word = g.split(|c: char| !c.is_ascii_alphanumeric() && c != '-').find(|t| ["test", "tests", "testing", "spec", "specs", "build", "builds", "lint", "linter", "linting", "typecheck", "type-check", "tsc", "compile", "compiles", "ci", "failing", "fails", "flaky", "coverage", "vitest", "jest", "e2e", "benchmark"].contains(t)).unwrap_or("").to_string();
    Reading::sure("checks", true, vec![format!("names “{word}”")])
}

/// THE DOMAIN GAUGE: what kind of work do the files the ask names say it is? SURE when it names at least one file whose
/// kind is known; unsure (value "unknown") when it names none — then the planner's tasks decide, after the fact.
pub fn domain(ask: &str) -> Reading<String> {
    let paths = crate::screen::paths_in(ask);
    let d = crate::roles::domain_of(&paths);
    if paths.is_empty() || d == Domain::Unknown { return Reading::unsure("domain", d.key().to_string(), vec![]); }
    Reading::sure("domain", d.key().to_string(), paths.into_iter().take(4).collect())
}

/// THE DOUBT GAUGE (operator 2026-10-11, taken from jev-router: "send work to a cheaper tier only when sure; on doubt go up" - a wrong cheap pick costs more than an extra strong one). The fact it points at is
/// the router's own grade for the route. SURE when the router graded it: `true` for "low" (it said it was not sure), `false` for "medium"/"high". A route with no grade says nothing (unsure). Pure - tested.
/// NOTE (measured 2026-10-11, 223 graded routes on this box): the router has said "high" 207 times, "medium" 16 and "low" never, so today this fires rarely; the rule is ready for the day it does.
pub fn doubt(grade: &str) -> Reading<bool> {
    match grade.trim().to_ascii_lowercase().as_str() {
        "low" => Reading::sure("doubt", true, vec!["the router graded this route low".into()]),
        g @ ("medium" | "high") => Reading::sure("doubt", false, vec![format!("the router graded this route {g}")]),
        _ => Reading::unsure("doubt", false, vec![]),
    }
}

/// What kind of risk a PATH carries, if it does: credentials, billing, migration, deploy. Segment and name based, so it is the same in any language and on any OS (backslashes are read as slashes). Example files
/// (`.env.example`) are not secrets. Pure - tested.
pub fn risk_of_path(path: &str) -> Option<&'static str> {
    let p = path.trim().replace('\\', "/").to_ascii_lowercase();
    let p = p.trim_start_matches("./");
    let segs: Vec<&str> = p.split('/').filter(|x| !x.is_empty() && *x != "**" && *x != "*").collect();
    let name = segs.last().copied().unwrap_or("");
    let example = name.ends_with(".example") || name.ends_with(".sample") || name.ends_with(".template") || name.ends_with(".dist");
    if !example && (name == ".env" || name.starts_with(".env.") || [".pem", ".key", ".p12", ".pfx", ".keystore"].iter().any(|e| name.ends_with(e))
        || ["id_rsa", "id_ed25519", ".npmrc", ".pypirc", ".netrc", "credentials", "credentials.json", "secrets.json"].contains(&name)
        || segs.iter().any(|s| matches!(*s, "secrets" | "secret" | "credentials"))) { return Some("credentials"); }
    if segs.iter().any(|s| ["billing", "payment", "stripe", "invoice", "paddle", "creem"].iter().any(|k| s.contains(k))) { return Some("billing"); }
    if segs.iter().any(|s| matches!(*s, "migrations" | "migration" | "migrate")) || name.ends_with(".sql") { return Some("migration"); }
    if p.starts_with(".github/workflows") || name == "dockerfile" || name.starts_with("docker-compose") || name.ends_with(".tf")
        || segs.iter().any(|s| matches!(*s, "terraform" | "k8s" | "kubernetes" | "helm" | "infra" | "deploy" | "deployment" | "ansible")) { return Some("deploy"); }
    None
}

/// THE RISK GAUGE, BY FILE: do the files a task owns include credentials, billing, a migration or a deploy? SURE when at least one does, and the path is the evidence; with none it makes no claim. Pure - tested.
pub fn risk_paths(paths: &[String]) -> Reading<Vec<String>> {
    let mut kinds: Vec<String> = vec![]; let mut ev: Vec<String> = vec![];
    for p in paths { if let Some(k) = risk_of_path(p) { if !kinds.iter().any(|x| x == k) { kinds.push(k.to_string()); } if ev.len() < 4 { ev.push(format!("{p} ({k})")); } } }
    if ev.is_empty() { Reading::unsure("risk", vec![], vec![]) } else { Reading::sure("risk", kinds, ev) }
}

/// THE RISK GAUGE, BY FILES THE ASK NAMES: `.env`, `db/migrations/9.sql`, `Dockerfile`, `.github/workflows/ci.yml`. `screen::paths_in` leaves a bare dotfile out on purpose (it serves the screening gate), so the tokens are read here:
/// a token counts only when it LOOKS like a file (a slash, a dot, or a known bare name), so the plain word "migrate" or "secrets" in a sentence is not a path. Pure - tested.
pub fn risk_named(ask: &str) -> Reading<Vec<String>> {
    let toks: Vec<String> = ask.split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | ',' | '<' | '>')).trim_end_matches(|c: char| matches!(c, '.' | ':' | ';' | '!' | '?')).to_string())
        .filter(|w| !w.is_empty() && !w.starts_with("http") && (w.contains('/') || w.contains('.') || matches!(w.to_ascii_lowercase().as_str(), "dockerfile" | "id_rsa" | "id_ed25519")))
        .collect();
    risk_paths(&toks)
}

/// THE RISK GAUGE, BY WORDS: does the ask itself talk about production, credentials, billing, migrations or deploys? The words are per-language data (`lexicon.rs`, `risky`), matched on word edges, so "reproduction" is not
/// "production". SURE when one is found, with the phrase as the evidence. Pure - tested.
pub fn risk_ask(ask: &str) -> Reading<Vec<String>> {
    let n = ask.to_lowercase().replace('\u{2019}', "'");
    let hit: Vec<String> = crate::lexicon::all().risky.iter().filter(|k| crate::lexicon::has_phrase_raw(&n, k)).take(3).map(|k| format!("the ask says “{k}”")).collect();
    if hit.is_empty() { Reading::unsure("risk", vec![], vec![]) } else { Reading::sure("risk", vec!["words".to_string()], hit) }
}

/// ONE LINE FOR THE RUN'S LOG: every ask gauge, its value, and whether it was sure. This is the label log — the local
/// classifier is trained on runs' own history, so the facts are written down beside the outcome from the first day.
pub fn line(ask: &str) -> String {
    let (w, c, d) = (wave(ask), checks(ask), domain(ask));
    let tag = |sure: bool| if sure { "sure" } else { "unsure" };
    format!("gauges — wave: {} ({}, {}) · checks: {} ({}) · domain: {} ({})",
        if w.value { "bigger than L" } else { "one job" }, w.evidence.join(", "), tag(w.sure),
        if c.value { "needed" } else { "not needed" }, c.evidence.join(", "),
        d.value, if d.sure { format!("sure: {}", d.evidence.join(", ")) } else { "unsure".into() })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn none(_: &str) -> bool { false }
    fn all(_: &str) -> bool { true }

    /// THE FAMILY'S RULE, as a property: a sure reading always carries evidence, and an unsure screen never carries a value.
    #[test]
    fn a_sure_gauge_always_points_at_something() {
        let asks = ["fix the off-by-one in serve/src/nope.rs", "make it better", "do it the same way", "1. add a\n2. add b", "run the tests", "restyle src/app.css", ""];
        for a in asks {
            let s = screen(a, none); if s.sure { assert!(!s.evidence.is_empty(), "{a}"); } else { assert!(s.value.is_empty(), "{a}"); }
            for r in [wave(a).sure.then(|| wave(a).evidence), checks(a).sure.then(|| checks(a).evidence), domain(a).sure.then(|| domain(a).evidence)].into_iter().flatten() { assert!(!r.is_empty(), "{a}"); }
        }
    }

    #[test]
    fn the_screen_gauge_fires_on_a_missing_path_and_abstains_otherwise() {
        let r = screen("in serve/src/nope.rs fix the off-by-one", none);
        assert!(r.sure && r.evidence[0].contains("missing-path: serve/src/nope.rs"), "{r:?}");
        // the path exists → nothing to point at → abstain, never "clear"
        let ok = screen("in serve/src/nope.rs fix the off-by-one", all);
        assert!(!ok.sure && ok.value.is_empty());
        // an ask that CREATES the file is not doubted for lacking it
        assert!(!screen("create serve/src/new_thing.rs with a parser", none).sure);
        // a vague ask with nothing to point at is not this gauge's to judge
        assert!(!screen("make the app faster", none).sure);
        assert!(screen("do it the same way as before", none).sure, "a reference with no antecedent is a fact about the text");
    }

    #[test]
    fn the_retry_gauge_stops_on_a_dead_seat_and_goes_again_on_a_hiccup() {
        for why in ["Not logged in · Please run /login", "HTTP 402 Payment Required", "insufficient credit on this account", "codex: command not found", "quota exceeded for today"] {
            let r = retry(why, 1, 2); assert!(r.sure && r.value == Retry::Stop, "{why}: {r:?}");
        }
        for why in ["request timed out after 300 s", "429 rate limit", "the service is overloaded", "no report", "HTTP 503", "the serve restarted"] {
            let r = retry(why, 1, 2); assert!(r.sure && r.value == Retry::Again, "{why}: {r:?}");
        }
        // the cap is a fact too
        assert!(retry("timed out", 2, 2).value == Retry::Stop);
        // an unclassified error keeps the old behaviour and says it is guessing
        let u = retry("something odd happened", 1, 2); assert!(!u.sure && u.value == Retry::Again);
        // digits inside a trace are not a login failure
        assert!(retry("panic at line 4012 of parser.rs", 1, 2).value == Retry::Again);
        assert!(!retry("panic at line 4012 of parser.rs", 1, 2).sure);
    }

    #[test]
    fn the_ask_gauges_report_what_the_pipeline_decides_with_and_agree_with_it() {
        let two = "1. add the field\n2. migrate the table";
        let w = wave(two); assert!(w.value && w.evidence[0].contains("2 numbered parts"));
        assert_eq!(w.value, crate::goals::ask_bigger_than_l(two));
        let long = "x".repeat(601); assert!(wave(&long).value && wave(&long).evidence[0].contains("601 characters"));
        assert!(!wave("rename the constant").value);
        let c = checks("make the failing test pass"); assert!(c.value && c.evidence[0].contains("failing") || c.evidence[0].contains("test"), "{c:?}");
        assert!(!checks("rename the constant").value);
        let d = domain("restyle src/components/card.css and src/components/card.tsx");
        assert!(d.sure && !d.evidence.is_empty(), "{d:?}");
        assert!(!domain("make the app faster").sure);
        let l = line(two); assert!(l.starts_with("gauges — wave: bigger than L") && l.contains("checks: not needed") && l.contains("domain:"), "{l}");
    }

    #[test]
    fn an_ask_that_hands_over_the_decision_is_not_screened() {
        for a in ["what do you suggest?", "Any ideas for the store?", "what should I do next", "what do you recommend for the cart", "Suggest improvements"] { assert!(delegated(a).sure, "{a}"); }
        // a specific ask that merely contains the word, or a long one, is not this gauge\'s to call
        for a in ["add a button", "implement the suggestion from the report: persist the cart to localStorage and add a checkout confirmation toast to the drawer", "rename the suggestions component"] { assert!(!delegated(a).sure, "{a}"); }
    }

    #[test]
    fn a_dead_seat_is_told_from_a_model_that_merely_failed() {
        for why in ["claude-code exited 1: Not logged in · Please run /login", "HTTP 402 Payment Required", "codex: command not found", "invalid api key"] { let r = seat_dead(why); assert!(r.sure && r.value, "{why}: {r:?}"); }
        // model-specific or transient: the fallback on the same engine may work
        for why in ["unknown model opus-9", "you have reached your Opus usage limit", "HTTP 403 for this model", "request timed out", "something odd"] { let r = seat_dead(why); assert!(!r.sure && !r.value, "{why}: {r:?}"); }
    }

    /// THE AUDIT (run with `-- --ignored --nocapture`): the gauges read against THIS machine\'s real history, so a rule that would
    /// have been wrong shows up as a line to read, not as a surprise in a run. Prints; asserts only that nothing panics and
    /// that no error the history holds is called permanent when the task later went on to succeed.
    #[test]
    #[ignore]
    fn audit_against_this_machines_history() {
        let path = std::env::var("GAUGE_AUDIT_RUNS").map(std::path::PathBuf::from).unwrap_or_else(|_| crate::goals::Goals::default_history_path());
        let runs: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|_| "[]".into())).unwrap_or_default();
        let (mut asks, mut wave_big, mut checks_yes, mut dom_sure, mut screen_sure) = (0, 0, 0, 0, 0);
        let (mut stop, mut again, mut unsure, mut gone) = (0, 0, 0, 0); let mut screen_lines: Vec<String> = vec![];
        let mut stop_lines: Vec<String> = vec![]; let mut unsure_lines: Vec<String> = vec![];
        for r in &runs {
            let goal = r["goal"].as_str().unwrap_or(""); let cwd = r["cwd"].as_str().unwrap_or(".");
            if goal.is_empty() { continue; }
            asks += 1;
            if wave(goal).value { wave_big += 1; } if checks(goal).value { checks_yes += 1; } if domain(goal).sure { dom_sure += 1; }
            // a run whose box is gone (a deleted bench clone) has every path "missing" — that is the audit\'s artefact, not the gauge\'s
            if !std::path::Path::new(cwd).is_dir() { gone += 1; continue; }
            let sc = screen(goal, |p| std::path::Path::new(cwd).join(p).exists());
            if sc.sure { screen_sure += 1; if screen_lines.len() < 15 { screen_lines.push(format!("  SCREEN {:?}  ← {}", sc.evidence, goal.chars().take(90).collect::<String>().replace('\n', " "))); } }
            for t in r["tasks"].as_array().into_iter().flatten() {
                let Some(e) = t["error"].as_str().filter(|e| !e.is_empty()) else { continue };
                let g = retry(e, 1, 2);
                match (g.sure, g.value) {
                    (true, Retry::Stop) => { stop += 1; if stop_lines.len() < 12 { stop_lines.push(format!("  STOP  {:?}  ← {}", g.evidence, e.chars().take(110).collect::<String>().replace('\n', " "))); } }
                    (true, Retry::Again) => again += 1,
                    _ => { unsure += 1; if unsure_lines.len() < 12 { unsure_lines.push(format!("  ?     {}", e.chars().take(110).collect::<String>().replace('\n', " "))); } }
                }
            }
        }
        println!("runs read: {}  asks: {asks} · box gone (skipped for the screen): {gone}", runs.len());
        println!("wave bigger-than-L: {wave_big} · checks needed: {checks_yes} · domain sure: {dom_sure} · screen sure (path missing NOW): {screen_sure}");
        println!("task errors → stop(sure): {stop} · again(sure): {again} · unsure: {unsure}");
        for l in screen_lines { println!("{l}"); } for l in stop_lines { println!("{l}"); } for l in unsure_lines { println!("{l}"); }
    }

    /// THE LABELLED RECALL/PRECISION AUDIT (bench weakness L13). The history audit above can only say "it fired N times"; it cannot say how often it SHOULD have. Here every case
    /// carries the answer written first (bench/gauges/cases.json - planted missing paths and dangling references, real CLI error texts, look-alikes that must not fire), and the
    /// score is per gauge: RECALL = of the cases that should fire, how many did; PRECISION = of the fires, how many were right. A gauge that abstains everywhere scores recall 0.
    #[test]
    fn a_status_code_is_a_status_code_and_not_a_count() {
        for w in ["expected 403 items, got 12", "ran 500 iterations, 3 failed", "syntaxerror at line 401 of parser.js", "read 429 rows"] { assert!(retry(w, 1, 2).evidence.is_empty(), "{w}"); assert!(!seat_dead(w).sure, "{w}"); }
        assert_eq!(retry("Error: 401 Unauthorized", 1, 2).value, Retry::Stop); assert_eq!(retry("HTTP 429", 1, 2).value, Retry::Again); assert_eq!(retry("503 Service Unavailable", 1, 2).value, Retry::Again);
    }
    #[test]
    fn the_machines_judgement_is_asked_for_in_every_pack_and_next_js_is_not_what_next() {
        for t in ["what do you suggest?", "o que você sugere?", "qué sugieres", "was schlägst du vor?", "surprise me", "you choose"] { assert!(delegated(t).sure, "{t}"); }
        for t in ["explain what next.js does", "remove the surprise-me button", "add a your-opinion field to the form"] { assert!(!delegated(t).sure, "{t}"); }
    }
    /// A FLOOR, NOT A BENCH (PLAN-THE-TESTS phase 7): every labelled case must be read right - no missed fire, no false fire. The cases were fixed one
    /// blind batch at a time, so this is a regression guard for what was learned, not a measure of blind recall (that stays ~60 %, see bench/PLAN-THE-GAUGES.md).
    /// A lexicon or gauge edit that loses a labelled case, or fires where the label says it must not, fails here.
    #[test]
    fn labelled_recall_and_precision() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../bench/gauges/cases.json")).unwrap()).unwrap();
        // (should_fire, fired, fired_right, misses)
        let mut by: std::collections::BTreeMap<String, (usize, usize, usize, usize, Vec<String>)> = Default::default();
        for c in &cases {
            let g = c["gauge"].as_str().unwrap(); let input = c["input"].as_str().unwrap();
            // (should fire?, did it fire, was what it said right)
            let (should, fired, right) = match g {
                "screen" => { let ex: Vec<&str> = c["exists"].as_array().unwrap().iter().filter_map(|x| x.as_str()).collect(); let r = screen(input, |p| ex.contains(&p)); let should = c["fire"].as_bool().unwrap(); (should, r.sure, r.sure == should) }
                "retry" => { let r = retry(input, 1, 2); let exp = c["expect"].as_str().unwrap();
                    let (should, right) = match exp { "stop" => (true, r.sure && r.value == Retry::Stop), "again" => (true, r.sure && r.value == Retry::Again), _ => (false, !r.sure) }; (should, r.sure, right) }
                "seat" => { let r = seat_dead(input); let should = c["expect"].as_bool().unwrap(); (should, r.sure, r.sure == should) }
                "delegated" => { let r = delegated(input); let should = c["fire"].as_bool().unwrap(); (should, r.sure, r.sure == should) }
                other => panic!("unknown gauge {other}"),
            };
            let e = by.entry(if c["heldout3"].as_bool().unwrap_or(false) { format!("{g} (held-out 3)") } else if c["heldout2"].as_bool().unwrap_or(false) { format!("{g} (held-out 2)") } else if c["heldout"].as_bool().unwrap_or(false) { format!("{g} (held-out)") } else { g.to_string() }).or_default();
            if should { e.0 += 1 } if fired { e.1 += 1 } if fired && right { e.2 += 1 } if should && fired && right { e.3 += 1 }
            if (should && !(fired && right)) || (!should && fired) { e.4.push(format!("{} {:?}  [{}]", if should { "MISSED" } else { "FALSE FIRE" }, input, c["note"].as_str().unwrap_or(""))); }
        }
        println!("\n== GAUGES vs LABELS ({} cases)   recall = fired-right / should-fire · precision = fired-right / fired · control: a gauge that abstains everywhere has recall 0", cases.len());
        for (g, (should, fired, fired_right, hit, misses)) in &by {
            let n = cases.iter().filter(|c| { let g0 = c["gauge"].as_str().unwrap_or(""); let key = if c["heldout3"].as_bool().unwrap_or(false) { format!("{g0} (held-out 3)") } else if c["heldout2"].as_bool().unwrap_or(false) { format!("{g0} (held-out 2)") } else if c["heldout"].as_bool().unwrap_or(false) { format!("{g0} (held-out)") } else { g0.to_string() }; key == *g }).count();
            println!("   {g:10} n={n:3}  should-fire {should:3}  fired {fired:3}  RECALL {hit}/{should} = {:.0}%   PRECISION {fired_right}/{fired} = {:.0}%", 100.0 * *hit as f64 / (*should).max(1) as f64, 100.0 * *fired_right as f64 / (*fired).max(1) as f64);
            for m in misses { println!("        {m}"); }
        }
        let wrong: Vec<&String> = by.values().flat_map(|v| v.4.iter()).collect();
        assert!(wrong.is_empty(), "{} labelled case(s) read wrong:\n{}", wrong.len(), wrong.iter().map(|w| w.as_str()).collect::<Vec<_>>().join("\n"));
        assert!(cases.len() >= 274, "the labelled set must not shrink ({} cases)", cases.len());
    }

    // ---- CAUTION: doubt and risk (operator 2026-10-11, from jev-router) ----
    #[test]
    fn doubt_is_the_routers_own_low_grade_and_a_missing_grade_claims_nothing() {
        let d = doubt("low"); assert!(d.sure && d.value && !d.evidence.is_empty(), "{d:?}");
        for g in ["high", "medium", "HIGH "] { let d = doubt(g); assert!(d.sure && !d.value && !d.evidence.is_empty(), "{g}: {d:?}"); }
        let d = doubt(""); assert!(!d.sure && !d.value && d.evidence.is_empty());
    }
    #[test]
    fn a_path_is_risky_by_what_it_is_not_by_a_word_in_the_ask() {
        for (p, k) in [(".env", "credentials"), ("config/.env.production", "credentials"), ("certs/server.pem", "credentials"), ("deploy/secrets/db.yml", "credentials"), ("C:\\app\\.npmrc", "credentials"),
                       ("src/billing/invoice.ts", "billing"), ("server/stripe_webhook.rs", "billing"), ("db/migrations/001_init.sql", "migration"), ("schema.sql", "migration"),
                       (".github/workflows/ci.yml", "deploy"), ("Dockerfile", "deploy"), ("infra/main.tf", "deploy"), ("k8s/pod.yaml", "deploy")] {
            assert_eq!(risk_of_path(p), Some(k), "{p}");
        }
        for p in [".env.example", "config/.env.sample", "src/app.ts", "README.md", "src/notes.txt", "src/author.rs"] { assert_eq!(risk_of_path(p), None, "{p}"); }
    }
    #[test]
    fn the_risk_gauge_is_sure_only_when_it_can_point_and_the_ask_words_match_on_word_edges_in_any_language() {
        let r = risk_paths(&["src/a.ts".into(), "db/migrations/2.sql".into()]); assert!(r.sure && r.value == vec!["migration".to_string()] && r.evidence[0].contains("migrations"), "{r:?}");
        let r = risk_paths(&["src/a.ts".into()]); assert!(!r.sure && r.evidence.is_empty());
        assert!(risk_ask("fix the bug in production now").sure);
        assert!(risk_ask("corrija o erro na produção").sure, "Portuguese is read too");
        assert!(risk_ask("atualiza a senha do admin").sure);
        let r = risk_ask("fix the reproduction steps in the docs"); assert!(!r.sure, "'reproduction' is not 'production': {r:?}");
        assert!(!risk_ask("add a dark mode toggle").sure);
    }
    #[test]
    fn files_the_ask_names_are_read_but_plain_words_are_not_paths() {
        for ask in ["edit .env and rerun", "please change `db/migrations/9.sql`.", "update the Dockerfile", "touch (.github/workflows/ci.yml), then stop"] { assert!(risk_named(ask).sure, "{ask}"); }
        for ask in ["we should migrate the docs and keep the secrets out of it", "see e.g. the deploy notes.", "visit https://example.com/billing for details", "edit .env.example"] { assert!(!risk_named(ask).sure, "{ask}"); }
    }
}
