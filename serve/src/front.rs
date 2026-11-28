//! THE FRONT OF HOUSE (operator 2026-09-29: "the agent conversation chat should be a single model using the same terminal instance and
//! never forget the conversation … it just asks Heimdall for the answer and writes the best way to the user, and gets the user input and
//! feeds Heimdall the best way — JSON, JEV style").
//!
//! What the operator's screenshot showed: `what do you suggest?` became a goal run; Heimdall asked, the operator answered, the run
//! reported — and then `go` reached a chat model that had never heard of any of it ("I'm in read-only antiChat mode … what would you
//! like to do?"). The conversation layer and the run layer shared nothing. This module is the wire between them, in three parts,
//! cheapest first:
//!
//!   1. CLOSED REPLIES ARE RESOLVED IN CODE. When a plan or a question is pending, `go` / `yes` / `cancel` / a chip's text are not
//!      language to interpret, they are state transitions. `resolve_closed` maps them onto approve / cancel / answer with no model,
//!      so they cannot be misread and cost nothing. (A gauge in the sense of gauge.rs: it acts only when it can point at the pending
//!      run it is answering.)
//!   2. THE CHAT SESSION REMEMBERS THE RUNS. `digest` is a compact, deterministic account of this pane's recent goal runs — what was
//!      asked, what Heimdall asked back and what was answered, the assumptions, the report — that `chat_pane::before_send` puts in
//!      front of the next turn, so "go" and "the second one" mean something to the model that reads them.
//!   3. AN OPEN REPLY BECOMES A TYPED INTENT. For a short, referring message ("go", "do the first one", "and the filtering?") after a
//!      run, a LOW model writes one JSON object from a fixed template; code lints it (`lint_intent`), and a rejected object goes back to
//!      the same low model with the exact errors to re-edit, twice at most. Low models are better at this than at prose. What survives is an
//!      `Intent` whose `ask` is a self-contained request Heimdall can plan; anything that never lints falls back to plain chat.
use crate::goals::{GoalRun, RunStatus};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Task, Approve, Cancel, Answer, Status, Question, Chat }
impl Kind {
    pub fn key(self) -> &'static str { match self { Kind::Task => "task", Kind::Approve => "approve", Kind::Cancel => "cancel", Kind::Answer => "answer", Kind::Status => "status", Kind::Question => "question", Kind::Chat => "chat" } }
    pub fn of(s: &str) -> Option<Kind> { [Kind::Task, Kind::Approve, Kind::Cancel, Kind::Answer, Kind::Status, Kind::Question, Kind::Chat].into_iter().find(|k| k.key() == s.trim().to_ascii_lowercase()) }
}

/// What the operator meant, in the only shapes the rest of the system accepts.
#[derive(Clone, Debug, PartialEq)]
pub struct Intent { pub kind: Kind, pub run: Option<i64>, pub ask: String, pub answer: String, pub why: String }
impl Intent { fn plain(kind: Kind) -> Intent { Intent { kind, run: None, ask: String::new(), answer: String::new(), why: String::new() } } }

/// The run the conversation is waiting on, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending { pub run: i64, pub awaiting: Awaiting, pub goal: String, pub questions: Vec<(String, Vec<String>)> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Awaiting { Approval, Answers }

/// THE NEWEST RUN OF THIS PANE THAT IS WAITING ON THE OPERATOR. Pure.
pub fn pending_of(runs: &[GoalRun], pane_id: i64) -> Option<Pending> {
    runs.iter().rev().filter(|r| r.pane_id == pane_id).find_map(|r| {
        let awaiting = match r.status { RunStatus::AwaitingApproval => Awaiting::Approval, RunStatus::AwaitingAnswers => Awaiting::Answers, _ => return None };
        Some(Pending { run: r.id, awaiting, goal: r.goal.clone(), questions: r.questions.iter().filter(|q| q.answer.is_none()).map(|q| (q.id.clone(), q.options.clone())).collect() })
    })
}

use crate::lexicon::{self, has_phrase, starts_with_word};
fn norm(text: &str) -> String { lexicon::norm(text) }
fn words_of(n: &str) -> usize { n.split(' ').filter(|w| !w.is_empty()).count() }

/// A REPLY THAT IS A STATE TRANSITION, NOT LANGUAGE (see the module doc, part 1). `None` = not a closed reply: interpret it some other
/// way. With a plan waiting, `go` approves it and `cancel` cancels it; with questions waiting, `go` takes the router's defaults, a chip's
/// exact text answers the one question that has it, and `cancel` cancels the run. Pure — tested.
pub fn resolve_closed(text: &str, pending: Option<&Pending>) -> Option<Intent> {
    let p = pending?; let n = norm(text);
    let intent = |kind: Kind, answer: String| Intent { kind, run: Some(p.run), ask: String::new(), answer, why: "resolved in code".into() };
    let yes = |p: &Pending| match p.awaiting { Awaiting::Approval => intent(Kind::Approve, String::new()), Awaiting::Answers => intent(Kind::Answer, String::new()) };
    // an emoji is a vote, in every language: 👍 goes, 🛑 cancels
    if n.is_empty() { return match lexicon::symbol_vote(text) { Some(true) => Some(yes(p)), Some(false) => Some(intent(Kind::Cancel, String::new())), None => None } }
    // A CHIP'S EXACT TEXT ANSWERS, AT ANY LENGTH (found in the browser, 2026-09-29: the chip "change nothing until I confirm the path" is seven words, the
    // five-word cap sent it down the verb rules, "change" read as an action, and it STARTED A NEW RUN while the question was still waiting). The cap protects the
    // yes/no words from long sentences; it has no business over a string the panel itself offered. Any language, any script: the chip is compared as text.
    // When SEVERAL open questions offer the same chip ("Other", "Keep as is"), it answers all of them - `front_op` applies it to each.
    if p.awaiting == Awaiting::Answers {
        if let Some((qid, opts)) = p.questions.iter().find(|(_, opts)| opts.iter().any(|o| norm(o) == n)) {
            let chip = opts.iter().find(|o| norm(o) == n).cloned().unwrap_or_default();
            return Some(Intent { kind: Kind::Answer, run: Some(p.run), ask: String::new(), answer: chip, why: format!("resolved in code: the chip of {qid}") });
        }
    }
    if words_of(&n) > 5 { return None }
    let l = lexicon::all();
    if l.no.contains(&n) { return Some(intent(Kind::Cancel, String::new())) }
    if l.yes.contains(&n) { return Some(yes(p)) }
    None
}

fn age_ms(now: i64, r: &GoalRun) -> i64 { now - r.ended.or(r.started).unwrap_or(r.created) }
const RECENT_MS: i64 = 30 * 60 * 1000;

/// THE CHAT'S MEMORY OF THE RUNS (module doc, part 2): this pane's runs from the last half hour, newest last, each as a few lines. Empty
/// when there are none. Deterministic — no model summarises it, so it cannot drift from what happened. Pure — tested.
pub fn digest(runs: &[GoalRun], pane_id: i64, now: i64) -> String {
    let mine: Vec<&GoalRun> = runs.iter().filter(|r| r.pane_id == pane_id && age_ms(now, r) <= RECENT_MS && r.status != RunStatus::Inline).collect();
    let take = mine.len().saturating_sub(3);
    let mut out = String::new();
    for r in &mine[take..] {
        out.push_str(&format!("- run #{} [{}] asked: \"{}\"\n", r.id, status_word(r.status), r.goal.chars().take(200).collect::<String>().replace('\n', " ")));
        for q in &r.questions {
            let a = q.answer.clone().filter(|a| !a.is_empty()).map(|a| format!("the operator answered: {a}")).unwrap_or_else(|| if q.default.is_empty() { "still open".into() } else { format!("still open (default: {})", q.default) });
            out.push_str(&format!("    Heimdall asked: {} — {a}\n", q.text.chars().take(200).collect::<String>()));
        }
        for a in r.assumptions.iter().take(3) { out.push_str(&format!("    assumed: {}\n", a.chars().take(200).collect::<String>())); }
        if let Some(rep) = r.report.as_ref().filter(|x| !x.trim().is_empty()) { out.push_str(&format!("    report: {}\n", rep.trim().chars().take(900).collect::<String>().replace('\n', "\n      "))); }
        if let Some(e) = r.error.as_ref().filter(|x| !x.is_empty()) { out.push_str(&format!("    error: {}\n", e.chars().take(200).collect::<String>())); }
    }
    if out.is_empty() { return String::new() }
    format!("[HEIMDALL'S RUNS IN THIS CONVERSATION — facts from the run log, oldest first. You are the voice: you do not plan or edit; when the operator wants work done, say you are handing it to Heimdall.]\n{out}")
}
fn status_word(s: RunStatus) -> &'static str {
    match s { RunStatus::Planning => "planning", RunStatus::AwaitingAnswers => "waiting for the operator's answers", RunStatus::AwaitingApproval => "plan waiting for the operator's go", RunStatus::Running => "running", RunStatus::Paused => "paused", RunStatus::Joining => "finishing", RunStatus::Done => "done", RunStatus::Failed => "failed", RunStatus::Cancelled => "cancelled", RunStatus::Inline => "inline" }
}

/// DID THE CHAT'S LAST WORD PROPOSE WORK AND WAIT FOR A GO? (found in the operator's conversation: the read-only chat answered "add filters" with
/// "say go: product filters by tag (+ search)", and then "go" - which has no run to refer to - went back to the same chat.) True when the newest
/// assistant turn in the log, with no operator turn after it, invites a go. Pure - tested.
pub fn proposal_pending<'a>(lines: impl Iterator<Item = &'a str>) -> bool {
    last_assistant(lines).map(|t| { let t = t.to_lowercase(); !proposals_of(&t).is_empty() || lexicon::PROPOSAL_CUES.iter().any(|k| t.contains(k)) }).unwrap_or(false)
}
/// the newest assistant text in a pane log with no operator turn after it (None when the operator spoke last, or nothing was said)
pub fn last_assistant<'a>(lines: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut last: Option<String> = None;
    for line in lines {
        let Ok(w) = serde_json::from_str::<Value>(line) else { continue };
        let ev = w.get("ev").cloned().unwrap_or(Value::Null);
        let ty = ev.get("type").and_then(Value::as_str).unwrap_or(""); let sub = ev.get("subtype").and_then(Value::as_str).unwrap_or("");
        if ty == "antiloki" && sub == "user" { last = None; }
        // the "which one?" note restates the proposals as `▸ proposal N:` lines, so the operator's "yes" (a user turn) does not lose them before the number arrives
        else if ty == "antiloki" && sub == "note" && ev.get("text").and_then(Value::as_str).map(|t| t.starts_with("▸ which one?")).unwrap_or(false) { last = ev.get("text").and_then(Value::as_str).map(String::from); }
        else if ty == "assistant" {
            let text = ev.get("message").and_then(|m| m.get("content")).and_then(Value::as_array).map(|a| a.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")).unwrap_or_default();
            if !text.trim().is_empty() { last = Some(text); }
        }
    }
    last
}

/// THE CHAT'S OWN PROPOSALS, AS DATA (operator 2026-09-29: the chat asked "what should we tackle next?", the operator said "yes", and the layer that should have known what
/// "yes" meant had nothing to read but prose). The chat is told (`PROPOSAL_RULE`) to write each concrete piece of work it suggests on a line of its own,
/// `▸ proposal: <the task, as the operator could send it>`. The line is in the pane's log, so the layer reads it back without any model, in any language: a bare
/// "yes" accepts a lone proposal, a number or an ordinal picks one of several, and a "yes" among several asks which - it never guesses. Pure - tested.
/// HOW THE CHAT TALKS ABOUT ITSELF (operator 2026-10-11, Windows: the first agent answered "hi" with "I'm in read-only mode ... I won't edit files or run mutating commands"). It is TRUE of how the chat session is wired (the conversation
/// layer never writes; Heimdall's runs do, in a safe copy, with a verifier) and WRONG about what the user can get done - and Cursor's own ask mode volunteers the sentence unprompted. So the chat is told what it is: the
/// voice in front of a team that does edit. Said every turn, in a few words, in front of the ask. Pure - tested.
pub const VOICE_RULE: &str = "You are antiloki's conversation layer. You talk, read and plan; when the user wants a change made, antiloki's Heimdall plans it, a worker edits in a safe copy of the project and it is checked before it lands, so say you will hand it over (or write the proposal as the rule below says). Never describe yourself as \"read-only\", \"in ask mode\" or unable to edit: that is how you are wired, not what the user can get done here.";
pub fn voice_rule(front: bool) -> &'static str { if front { VOICE_RULE } else { "" } }

pub const PROPOSAL_RULE: &str = "When you suggest work for Heimdall to do, write each concrete suggestion on a line of its own starting with `▸ proposal:` followed by the task as one imperative sentence the operator could send as it is (at most three). Do not tell the operator how to send it.";
pub fn proposals_of(text: &str) -> Vec<String> {
    text.lines().filter_map(|l| {
        let t = l.trim().trim_start_matches(|c: char| matches!(c, '▸' | '>' | '*' | '-' | '•' | ' ' | '\t' | '_' | '`')).trim();
        let low = t.to_lowercase();
        let rest = low.strip_prefix("proposal")?;
        let after = &t[t.len() - rest.len()..];
        let after = after.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '#').trim_start();
        let body = after.strip_prefix(':').or_else(|| after.strip_prefix('-'))?.trim().trim_matches(|c: char| matches!(c, '*' | '_' | '`')).trim();
        if body.chars().count() < 3 { None } else { Some(body.chars().take(300).collect()) }
    }).take(3).collect()
}
/// what the operator's short reply does to the proposals on the table
#[derive(Debug, PartialEq)]
pub enum Pick { One(String), Ask,
    /// a bare yes with several proposals that are VARIANTS of one piece of work: the first (the chat's own lead), with how many there were so the thread can say "say 2 or 3 to switch"
    First(String, usize) }
/// ARE THESE PROPOSALS ONE PIECE OF WORK SAID SEVERAL WAYS? (operator 2026-10-09: "the buttons look flat" -> three proposals about the buttons -> "yes" -> "which one?"). Yes when one distinctive word (5+ letters,
/// plural folded, generic page-and-styling words left out) appears in EVERY proposal - "buttons" in all three. A dark-theme toggle and a sort dropdown share nothing, so a yes between them still asks. Pure - tested.
pub fn proposals_are_variants(props: &[String]) -> bool {
    if props.len() < 2 { return false }
    const GENERIC: &[&str] = &["should", "would", "could", "their", "there", "which", "where", "while", "these", "those", "other", "proposal", "keeping", "existing", "behavior", "behaviour", "style", "styles", "styling", "design", "page", "thing", "things", "using", "about", "within", "without", "across", "every", "stays", "state", "states", "layout", "polish", "better", "cleaner", "simple", "simpler", "modern"];
    let words = |t: &str| -> std::collections::HashSet<String> {
        t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 5 && !GENERIC.contains(w)).map(|w| w.strip_suffix('s').filter(|_| w.chars().count() > 5).unwrap_or(w).to_string()).collect()
    };
    let sets: Vec<_> = props.iter().map(|p| words(p)).collect();
    sets[0].iter().any(|w| sets[1..].iter().all(|s| s.contains(w)))
}
pub fn pick_proposal(text: &str, props: &[String]) -> Option<Pick> {
    if props.is_empty() { return None }
    let n = norm(text);
    let yes = lexicon::all().yes.contains(&n) || lexicon::symbol_vote(text) == Some(true);
    // "2", "2.", "option 2", "#2" - a digit that names one of the proposals
    let digit = n.split(' ').filter_map(|w| w.parse::<usize>().ok()).next().filter(|k| *k >= 1 && *k <= props.len());
    const ORD: [&[&str]; 3] = [&["first", "primeiro", "primeira", "primero", "primera", "premier", "première", "erste", "ersten", "primo", "prima", "1st"], &["second", "segundo", "segunda", "deuxième", "deuxieme", "zweite", "zweiten", "secondo", "seconda", "2nd"], &["third", "terceiro", "terceira", "tercero", "tercera", "troisième", "troisieme", "dritte", "dritten", "terzo", "terza", "3rd"]];
    let ord = (0..props.len().min(3)).find(|k| n.split(' ').any(|w| ORD[*k].contains(&w)));
    if let Some(k) = digit.map(|d| d - 1).or(ord) { if words_of(&n) <= 4 { return Some(Pick::One(props[k].clone())) } }
    if yes { return Some(if props.len() == 1 { Pick::One(props[0].clone()) } else if proposals_are_variants(props) { Pick::First(props[0].clone(), props.len()) } else { Pick::Ask }) }
    None
}

/// SHOULD A MODEL READ THIS MESSAGE? Only a short, referring message right after a run — "go", "do the first one", "and the filtering?" —
/// needs its context resolved. Everything else keeps the ordinary road, at no extra call. Pure — tested.
pub fn wants_voice(text: &str, runs: &[GoalRun], pane_id: i64, now: i64, proposal: bool) -> bool {
    let n = norm(text); let words = words_of(&n);
    if words == 0 || words > 12 { return false }
    // something to refer back TO: a recent run, or a proposal the chat itself just made ("say go to run it")
    if !proposal && !runs.iter().any(|r| r.pane_id == pane_id && age_ms(now, r) <= RECENT_MS && r.status != RunStatus::Inline) { return false }
    let first = n.split(' ').next().unwrap_or("");
    // a message that REFERS BACK: it opens with a referring word, or it is one word. "add authentication" is a new ask in two words - it keeps
    // the ordinary road (found live: the voice rewrote it into a longer ask the operator never typed).
    words == 1 || lexicon::all().follow_ups.iter().any(|w| starts_with_word(&n, w)) || { let _ = first; false }
}

/// HAS THIS DIGEST ALREADY BEEN SAID TO THIS CHAT? The session resumes with its own history, so the same digest twice is only noise: it rides
/// in front of a turn when it CHANGED since the last one sent to this pane (a run moved, a question was answered, a report landed).
pub fn fresh(pane_id: i64, digest: &str) -> bool {
    use std::hash::{Hash, Hasher};
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<i64, u64>>> = std::sync::OnceLock::new();
    let mut h = std::collections::hash_map::DefaultHasher::new(); digest.hash(&mut h); let now = h.finish();
    let m = SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut g = m.lock().unwrap_or_else(|p| p.into_inner());
    if g.get(&pane_id) == Some(&now) { return false }
    g.insert(pane_id, now); true
}

/// WHAT THE CONVERSATION HOLDS, for classifying a message: nothing pending, or a plan/questions waiting; a run recently finished or still running; a proposal
/// the chat just made.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ctx { pub recent_run: bool, pub running: bool, pub proposal: bool }

/// the message with a leading polite frame ("could you", "por favor", "kannst du") taken off
fn strip_polite(n: &str) -> String { for p in &lexicon::all().polite { if starts_with_word(n, p) { return n.strip_prefix(p.as_str()).unwrap_or(n).trim().to_string() } } n.to_string() }
/// does an action verb (any language, one word or a phrase) open the text, with something left to act on?
fn opens_with_action(bare: &str) -> bool { lexicon::all().actions.iter().any(|a| starts_with_word(bare, a) && bare.chars().count() > a.chars().count()) }
fn opens_with(bare: &str, list: &[String]) -> bool { list.iter().any(|w| starts_with_word(bare, w)) }
/// text that is NOT prose to be classified: a slash command, a shell/bang line, a mention, a markdown heading; or a paste (a stack trace, a diff, a file)
fn is_structural(text: &str) -> bool {
    let t = text.trim_start();
    matches!(t.chars().next(), Some('/' | '!' | '@' | '#' | '$' | '>')) || t.chars().count() > 600 || t.lines().count() > 6
}

/// THE DETERMINISTIC CLASSIFIER (front mode): sure ONLY when the words point at the answer - an action verb, a question opener, a pleasantry, a status phrase,
/// a symptom. `None` = not decided here (a bare noun, a message that refers back to something, a language with no pack): the low model writes the typed intent.
/// LANGUAGE-AGNOSTIC (operator 2026-09-29): structure is decided without words (a slash or paste is never prose; a question mark in any script asks; a message of
/// only symbols is chat); the words come from every language pack at once (`lexicon.rs`). Rules run in an order that matters: a referring message after a run goes
/// to the voice BEFORE the verb/question rules, so "implement that" is not planned blind. Pure - benched on labelled messages (bench/intent).
pub fn classify_open(text: &str, ctx: Ctx) -> Option<Kind> {
    if is_structural(text) { return None }
    let n = norm(text);
    if n.is_empty() { return if text.trim().is_empty() { None } else { Some(Kind::Chat) } }
    let l = lexicon::all();
    let words = words_of(&n);
    let bare = strip_polite(&n);
    let asks = text.trim_end().ends_with(&lexicon::QUESTION_MARKS[..]) || text.trim_start().starts_with('¿');
    let opener = opens_with(&bare, &l.openers);
    let suggests = opens_with(&bare, &l.suggestion);
    let referring = (ctx.recent_run || ctx.proposal) && (words <= 4 && opens_with(&n, &l.referring)
        || l.referents.iter().any(|r| has_phrase(&bare, r)) && (opens_with_action(&bare) || opens_with(&bare, &l.referring) || opener));
    if l.pleasantries.contains(&n) { return Some(Kind::Chat) }
    if l.status.iter().any(|s| has_phrase(&n, s)) { return Some(if ctx.running || ctx.recent_run { Kind::Status } else { Kind::Chat }) }
    if crate::gauge::delegated(text).sure { return Some(Kind::Task) }
    // "what about the sorting?" / "how about a dark mode?": a suggestion or a question about something said before - the model reads it, not a verb rule
    if suggests { return None }
    if referring && !opener { return None }
    if opens_with_action(&bare) && !(asks && opener) { return Some(Kind::Task) }
    // "In src/products.ts, change formatPrice to…" / "on the cart page make the total bold": a target first, then the verb. The verb has to come within the first
    // six words, so "in which file is the cart kept" (no verb) and "in the products file, what does formatPrice do" (an opener) are not swept in.
    { let toks: Vec<&str> = bare.split(' ').collect();
      if lexicon::TARGET_PREPOSITIONS.contains(&toks[0]) && toks.len() >= 4 && !l.openers.iter().any(|q| has_phrase(&toks[1..toks.len().min(6)].join(" "), q))
          && { let head = toks[1..toks.len().min(7)].join(" "); l.actions.iter().any(|a| has_phrase(&head, a)) } { return Some(Kind::Task) } }
    if opener || asks { return Some(Kind::Question) }
    if opens_with_action(&bare) { return Some(Kind::Task) }
    if l.symptoms.iter().any(|s| has_phrase(&n, s)) { return Some(Kind::Task) }
    None
}

// ── THE TYPED INTENT: one template, one lint, a bounded re-edit (module doc, part 3) ─────────────────────────────────

/// the fields, in order — the template shows exactly these and the lint refuses anything else
pub const FIELDS: [&str; 5] = ["kind", "run", "ask", "answer", "why"];

/// THE LINT. Every reason it gives is written to be fed back to the model that wrote the object. Pure — tested.
pub fn lint_intent(reply: &str, pending: Option<&Pending>, known_runs: &[i64]) -> Result<Intent, Vec<String>> {
    let (s0, e0) = (reply.find('{'), reply.rfind('}'));
    let v: Value = match (s0, e0) { (Some(a), Some(b)) if b > a => match serde_json::from_str(&reply[a..=b]) { Ok(v) => v, Err(e) => return Err(vec![format!("not valid JSON ({e}) — reply with ONE JSON object and nothing else")]) },
                                    _ => return Err(vec!["no JSON object found — reply with ONE JSON object and nothing else".into()]) };
    let Some(o) = v.as_object() else { return Err(vec!["the reply must be a JSON object".into()]) };
    let mut errs: Vec<String> = vec![];
    for k in o.keys() { if !FIELDS.contains(&k.as_str()) { errs.push(format!("unknown field \"{k}\" — the fields are exactly {}", FIELDS.join(", "))); } }
    let text = |k: &str| o.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    let Some(kind) = Kind::of(&text("kind")) else { errs.push("\"kind\" must be one of task, approve, cancel, answer, status, question, chat".into()); return Err(errs) };
    let run = o.get("run").and_then(|r| r.as_i64().or_else(|| r.as_str().and_then(|x| x.trim_start_matches('#').parse().ok())));
    let (ask, answer, why) = (text("ask"), text("answer"), text("why"));
    match kind {
        Kind::Task => {
            if ask.split_whitespace().count() < 3 { errs.push("\"ask\" must be a self-contained request of at least 3 words for Heimdall (say what to build or change, using what the conversation settled), not \"go\" or \"yes\"".into()); }
            if ask.chars().count() > 700 { errs.push("\"ask\" must be at most 700 characters".into()); }
        }
        Kind::Approve | Kind::Cancel | Kind::Answer => {
            match (run, pending) {
                (None, _) => errs.push(format!("\"run\" must be the number of the run this is about (pending: {})", pending.map(|p| p.run.to_string()).unwrap_or_else(|| "none".into()))),
                (Some(r), Some(p)) if r != p.run => errs.push(format!("\"run\" {r} is not the run waiting for the operator (it is {})", p.run)),
                (Some(r), None) if !known_runs.contains(&r) => errs.push(format!("\"run\" {r} is not a run in this conversation")),
                (Some(_), None) => errs.push(format!("nothing is waiting for the operator, so \"{}\" has nothing to act on — use \"task\" or \"chat\"", kind.key())),
                _ => {}
            }
            if kind == Kind::Approve && pending.map(|p| p.awaiting) != Some(Awaiting::Approval) { errs.push("no plan is waiting for approval".into()); }
            if kind == Kind::Answer {
                let opts: Vec<&String> = pending.map(|p| p.questions.iter().flat_map(|(_, o)| o.iter()).collect()).unwrap_or_default();
                if !answer.is_empty() && !opts.is_empty() && !opts.iter().any(|o| norm(o) == norm(&answer)) && answer.chars().count() > 200 { errs.push("\"answer\" must be short or one of the offered chips".into()); }
            }
        }
        Kind::Status | Kind::Question | Kind::Chat => { if !ask.is_empty() { errs.push("\"ask\" must be empty unless kind is task".into()); } }
    }
    if why.chars().count() > 160 { errs.push("\"why\" must be one short sentence (160 characters at most)".into()); }
    if !errs.is_empty() { return Err(errs) }
    Ok(Intent { kind, run, ask, answer, why })
}

/// The template's inputs, rendered by `prompts.rs` ("front.intent"). `previous` carries a rejected reply and its errors for the re-edit.
pub fn intent_prompt(text: &str, digest: &str, pending: Option<&Pending>, previous: Option<(&str, &[String])>) -> String {
    let pend = match pending {
        Some(p) => format!("run #{} is {} — {}", p.run, if p.awaiting == Awaiting::Approval { "a plan waiting for the operator's approval" } else { "waiting for the operator's answers" },
                           p.questions.iter().map(|(id, o)| format!("{id} chips: [{}]", o.join(" | "))).collect::<Vec<_>>().join("; ")),
        None => String::new(),
    };
    let prev = previous.map(|(r, errs)| format!("YOUR PREVIOUS REPLY WAS REJECTED.\nPrevious reply: {r}\nWhat is wrong: {}\nEdit it: keep what was right, fix exactly what is wrong.", errs.join(" · "))).unwrap_or_default();
    crate::prompts::render_base("front.intent", &crate::goals::vars(&[("text", json!(text)), ("digest", json!(digest)), ("pending", json!(pend)), ("previous", json!(prev))]))
}

/// WRITE IT, LINT IT, RE-EDIT IT (twice at most), with whatever model call the caller supplies — a low model in production, a script in
/// the tests. Returns the intent, where it came from, and the lint errors seen on the way. Never fails: a reply that will not lint is
/// `chat`, i.e. the ordinary road. Async because the model call is.
pub async fn interpret<F, Fut>(text: &str, digest: &str, pending: Option<&Pending>, known_runs: &[i64], ask_model: F) -> (Intent, &'static str, Vec<String>)
where F: Fn(String) -> Fut, Fut: std::future::Future<Output = Result<String, String>> {
    let mut seen: Vec<String> = vec![]; let mut prev: Option<(String, Vec<String>)> = None;
    for attempt in 0..3 {
        let prompt = intent_prompt(text, digest, pending, prev.as_ref().map(|(r, e)| (r.as_str(), e.as_slice())));
        let reply = match ask_model(prompt).await { Ok(r) => r, Err(e) => { seen.push(format!("model call failed: {e}")); break } };
        match lint_intent(&reply, pending, known_runs) {
            Ok(i) => return (i, if attempt == 0 { "model" } else { "model, re-edited" }, seen),
            Err(errs) => { seen.extend(errs.iter().cloned()); prev = Some((reply.chars().take(600).collect(), errs)); }
        }
    }
    (Intent::plain(Kind::Chat), "fallback", seen)
}

// ── THE ENDPOINT: the input box asks this before it does anything else ────────────────────────────────────────────────

/// THE LOW MODEL THAT WRITES THE INTENT: the cheap decision seat the record names for `screening` (Haiku, Gemini…), at the low tier, two
/// turns, forty-five seconds - it writes one small JSON object and is asked to re-edit it, nothing more.
/// WHICH MODEL WRITES THE INTENT. The screening winner's binding can carry an EMPTY model (a Cursor pane found live 2026-09-29: "'' is not one of Cursor's own models" - the call
/// failed, the layer gave up, and the operator's "yes" went raw to a chat that had to guess). A bound model is used only if it is named and not refused; else the seat's own
/// cheapest rung; else nothing (the caller falls back to plain chat, and says so). Pure - tested.
pub fn choose_low_model(bound: Option<&str>, rung: Option<&str>, refused: impl Fn(&str) -> bool) -> Option<String> {
    [bound, rung].into_iter().flatten().map(str::trim).find(|m| !m.is_empty() && !refused(m)).map(String::from)
}
async fn low_model(engine_key: &str, cwd: &str, prompt: String) -> Result<String, String> {
    let pane_engine = crate::cli::engine_of(&json!(engine_key)).ok_or_else(|| format!("no such engine: {engine_key}"))?;
    // the seat chosen in Settings > Seats plans and decides (cli::heimdall_engine); the pane's own engine only when that gives nothing
    let seat = crate::cli::heimdall_engine(Some(pane_engine)).unwrap_or(pane_engine);
    let b = crate::cli::job_binding_any(crate::cli::Job::Screening, seat, None).0;
    let model = choose_low_model(b.model.as_deref(), crate::cli::tier_model(b.engine, crate::cli::Tier::Low).as_deref(), |m| crate::cli::model_blocked(b.engine, Some(m)).is_some())
        .ok_or_else(|| format!("no usable low model on {}", b.engine.key()))?;
    let opts = crate::cli::RunOpts { max_turns: Some(2), timeout_ms: Some(25_000), model: Some(model), tier: Some(crate::cli::Tier::Low), template: Some("front.intent".into()), ..Default::default() };
    let out = crate::cli::run_cli(b.engine, &[crate::cli::ChatMessage { role: "user".into(), content: prompt }], cwd, true, opts, None).await?;
    Ok(out.parsed.content)
}

/// `POST /api/v1/cockpit/agent/front {pane_id, text}` -> `{handled, kind, run?, ask?, source}`.
///   handled:true  - the serve did it (approved / cancelled / answered a run) and echoed the operator's words; the caller stops here.
///   handled:false - carry on down the ordinary road; `kind:"task"` brings a `ask` that replaces the typed text (the self-contained
///                   request the conversation settled), anything else changes nothing.
pub async fn front_op(state: std::sync::Arc<crate::dispatch::AppState>, reg: std::sync::Arc<crate::agent::AgentRegistry>, goals: std::sync::Arc<crate::goals::Goals>, body: axum::body::Bytes) -> axum::response::Response {
    use axum::response::IntoResponse;
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let pane_id = b.get("pane_id").and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0);
    let text = b.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let done = |v: Value| axum::Json(v).into_response();
    if pane_id == 0 || text.is_empty() { return done(json!({ "handled": false })) }
    let runs = goals.list(None); let now = crate::cli::now_ms();
    let pending = pending_of(&runs, pane_id);
    let known: Vec<i64> = runs.iter().filter(|r| r.pane_id == pane_id).map(|r| r.id).collect();
    let mut source = "code";
    // the pane's own log, once, for "did the chat just propose work?"
    let proposal = { let cwd = crate::chat_pane::pane_row(&state, pane_id).await.map(|p| p.get("worktree_path").and_then(Value::as_str).unwrap_or(".").to_string()).unwrap_or_else(|| ".".into()); proposal_pending(crate::chat_pane::events_text(&cwd, pane_id).lines()) };
    // THE CHAT'S OWN PROPOSALS (module doc, part 4): a bare yes, a number or an ordinal against the `▸ proposal:` lines of its last message - no model
    if pending.is_none() {
        let cwd = crate::chat_pane::pane_row(&state, pane_id).await.map(|p| p.get("worktree_path").and_then(Value::as_str).unwrap_or(".").to_string()).unwrap_or_else(|| ".".into());
        let props = last_assistant(crate::chat_pane::events_text(&cwd, pane_id).lines()).map(|t| proposals_of(&t)).unwrap_or_default();
        match pick_proposal(&text, &props) {
            Some(Pick::One(p)) => {
                reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "dim", "text": format!("▸ understood as: {p}") }), true);
                return done(json!({ "handled": false, "kind": "task", "ask": p, "source": "code", "why": "the chat's own proposal, accepted" }));
            }
            Some(Pick::First(p, n)) => {
                reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "dim", "text": format!("▸ taking proposal 1 of {n} - they are the same piece of work (say 2{} to switch): {p}", if n > 2 { " or 3" } else { "" }) }), true);
                return done(json!({ "handled": false, "kind": "task", "ask": p, "source": "code", "why": "the first of the chat's proposals, which are variants of one piece of work" }));
            }
            Some(Pick::Ask) => {
                reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "user", "text": text }), true);
                reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "dim", "text": format!("▸ which one? say the number\n{}", props.iter().enumerate().map(|(i, p)| format!("▸ proposal {}: {p}", i + 1)).collect::<Vec<_>>().join("\n")) }), true);
                return done(json!({ "handled": true, "kind": "ask", "source": "code" }));
            }
            None => {}
        }
    }
    let mut intent = resolve_closed(&text, pending.as_ref());
    if intent.is_none() && wants_voice(&text, &runs, pane_id, now, proposal) {
        let pane = crate::chat_pane::pane_row(&state, pane_id).await.unwrap_or(Value::Null);
        let (engine, cwd) = (pane.get("pane_kind").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| crate::cli::heimdall_engine(None).map_or("claude-code", |e| e.key()).to_string()), pane.get("worktree_path").and_then(Value::as_str).unwrap_or(".").to_string());
        let brief = crate::chat_pane::conversation_brief(&cwd, pane_id, &engine, 6_000);
        let dg = format!("{brief}{}", digest(&runs, pane_id, now));
        let (i, src, seen) = interpret(&text, &dg, pending.as_ref(), &known, |p| { let (e, c) = (engine.clone(), cwd.clone()); async move { low_model(&e, &c, p).await } }).await;
        if !seen.is_empty() { reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "dim", "text": format!("front: the intent was {} after {} lint note{} ({})", if src == "fallback" { "left to plain chat" } else { "re-edited" }, seen.len(), if seen.len() == 1 { "" } else { "s" }, seen.join(" · ").chars().take(240).collect::<String>()) }), true); }
        intent = Some(i); source = src;
    }
    // NOT A CLOSED REPLY AND NOT A REFERRING ONE: the deterministic classifier (sure only where the words point at the answer) says what kind of message it is.
    // The input honours a sure "task" (Heimdall takes it however short) and a sure "chat" (an acknowledgement is not planned); a "question" is reported and the
    // existing road is kept until the parity work is done. `None` = not decided here.
    if intent.is_none() {
        let ctx = Ctx { recent_run: runs.iter().any(|r| r.pane_id == pane_id && age_ms(now, r) <= RECENT_MS && r.status != RunStatus::Inline),
                        running: runs.iter().any(|r| r.pane_id == pane_id && matches!(r.status, RunStatus::Running | RunStatus::Planning | RunStatus::Joining)), proposal };
        return match classify_open(&text, ctx) { Some(k) => done(json!({ "handled": false, "kind": k.key(), "source": "code" })), None => done(json!({ "handled": false })) };
    }
    let Some(i) = intent else { return done(json!({ "handled": false })) };
    match i.kind {
        Kind::Task => {
            // SAY WHAT HEIMDALL WAS TOLD: the operator typed a few words and the run is asked in a full sentence - the thread shows the guess
            if i.ask.trim() != text { reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "dim", "text": format!("▸ understood as: {}", i.ask.chars().take(300).collect::<String>()) }), true); }
            done(json!({ "handled": false, "kind": "task", "ask": i.ask, "source": source, "why": i.why }))
        }
        Kind::Chat | Kind::Status | Kind::Question => done(json!({ "handled": false, "kind": i.kind.key(), "source": source })),
        Kind::Approve | Kind::Cancel | Kind::Answer => {
            let Some(run) = i.run else { return done(json!({ "handled": false })) };
            reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "user", "text": text }), true);
            let note = |t: String, tone: &str| reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": tone, "text": t }), true);
            match i.kind {
                Kind::Approve => { let ok = goals.approve(run).is_some(); note(if ok { format!("▸ plan approved — run #{run} is running") } else { format!("▸ run #{run} could not be approved") }, if ok { "ok" } else { "warn" }); }
                Kind::Cancel => { let ok = goals.cancel(run).is_some(); note(if ok { format!("▸ run #{run} cancelled") } else { format!("▸ run #{run} could not be cancelled") }, if ok { "dim" } else { "warn" }); }
                _ => {
                    let answers = match (&pending, i.answer.is_empty()) { (Some(p), false) if p.questions.len() == 1 => json!({ p.questions[0].0.clone(): i.answer.clone() }), (Some(p), false) => { let mut m = serde_json::Map::new(); for (q, o) in &p.questions { if o.iter().any(|x| norm(x) == norm(&i.answer)) { m.insert(q.clone(), json!(i.answer.clone())); } } if m.is_empty() { m.insert(p.questions[0].0.clone(), json!(i.answer.clone())); } Value::Object(m) } _ => json!({}) };
                    let ok = goals.answer(run, &answers).await.is_some();
                    note(if ok { format!("▸ answered run #{run}{} — planning continues", if i.answer.is_empty() { " with the defaults".to_string() } else { format!(": {}", i.answer) }) } else { format!("▸ run #{run} could not be answered") }, if ok { "ok" } else { "warn" });
                }
            }
            done(json!({ "handled": true, "kind": i.kind.key(), "run": run, "source": source }))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_chat_is_told_it_is_the_voice_of_a_team_that_edits_and_never_to_call_itself_read_only() {
        let v = voice_rule(true);
        assert!(v.contains("Heimdall") && v.contains("hand it over"), "{v}");
        // it names the phrases only to forbid them
        assert!(v.contains("Never describe yourself as"), "{v}");
        assert_eq!(voice_rule(false), "", "a chat that is not in front mode (it writes itself) is told nothing");
        assert!(v.chars().count() < 600, "a rule said every turn is short");
    }
    use super::*;
    fn pend(awaiting: Awaiting) -> Pending { Pending { run: 7, awaiting, goal: "what do you suggest?".into(), questions: vec![("q1".into(), vec!["Add product discovery and filtering".into(), "Fix accessibility".into(), "Persist the cart".into()])] } }


    /// THE INTENT BENCH (bench/intent/cases.json, 110 labelled messages). Run: `cargo test -p antiloki-serve --lib intent_bench -- --ignored --nocapture`.
    /// Deterministic layer alone; set INTENT_MODEL=1 to let the real low model write the typed intent for whatever the classifier defers.
    #[tokio::test]
    #[ignore]
    async fn intent_bench() {
        let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../bench/intent/cases.json")).unwrap()).unwrap();
        let mk_pending = |x: &str| -> Option<Pending> { match x { "approval" => Some(Pending { run: 7, awaiting: Awaiting::Approval, goal: "g".into(), questions: vec![] }),
            "answers" => Some(Pending { run: 7, awaiting: Awaiting::Answers, goal: "g".into(), questions: vec![("q1".into(), vec!["Main App component".into(), "A new component".into()])] }), _ => None } };
        let (mut decided, mut right_decided, mut deferred) = (0, 0, vec![]); let mut wrong: Vec<String> = vec![];
        let mut by_kind: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
        let mut all: Vec<(Value, Option<Kind>)> = vec![];
        for c in &cases {
            let (t, x, want) = (c["t"].as_str().unwrap(), c["x"].as_str().unwrap(), c["k"].as_str().unwrap());
            let ctx = Ctx { recent_run: matches!(x, "run" | "running"), running: x == "running", proposal: x == "proposal" };
            let got = resolve_closed(t, mk_pending(x).as_ref()).map(|i| i.kind).or_else(|| classify_open(t, ctx));
            all.push((c.clone(), got));
            match got {
                Some(k) => { decided += 1; let ok = k.key() == want; if ok { right_decided += 1 } else { wrong.push(format!("{t:?} [{x}] wanted {want}, got {}", k.key())) }
                             let e = by_kind.entry(want.into()).or_default(); e.0 += 1; if ok { e.1 += 1 } }
                None => deferred.push((t.to_string(), x.to_string(), want.to_string())),
            }
        }
        println!("\n== DETERMINISTIC LAYER: {} cases · decided {} ({:.0}%) · right when decided {}/{} ({:.1}%)", cases.len(), decided, 100.0 * decided as f64 / cases.len() as f64, right_decided, decided, 100.0 * right_decided as f64 / decided.max(1) as f64);
        for (k, (n, r)) in &by_kind { println!("   {k:9} decided {n:3}  right {r:3}"); }
        println!("   WRONG when decided:"); for w in &wrong { println!("     ✗ {w}"); }
        println!("   DEFERRED to the voice ({}):", deferred.len()); for (t, x, w) in &deferred { println!("     · {t:?} [{x}] wanted {w}"); }
        if std::env::var("INTENT_MODEL").ok().as_deref() == Some("1") {
            let (mut right, mut total, mut miss) = (0, 0, vec![]);
            for (t, x, w) in &deferred {
                let pend = mk_pending(x);
                let dg = match x.as_str() { "run" | "proposal" | "running" => "- run #1 [done] asked: \"what do you suggest?\"\n    report: 1. Persist the cart to localStorage  2. Add product filtering by tag  3. Accessibility: focus rings on buttons\n".to_string(), _ => String::new() };
                let (i, src, _) = interpret(t, &dg, pend.as_ref(), &[1, 7], |p| async move { low_model("claude-code", ".", p).await }).await;
                total += 1; if i.kind.key() == w { right += 1 } else { miss.push(format!("{t:?} [{x}] wanted {w}, the voice said {} ({src})", i.kind.key())) }
            }
            println!("== THE VOICE on the {} deferred: right {}/{}", total, right, total); for m in &miss { println!("     ✗ {m}"); }
            println!("== OVERALL (layer + voice): {}/{} = {:.1}%", right_decided + right, cases.len(), 100.0 * (right_decided + right) as f64 / cases.len() as f64);
        }
    }

    /// A FLOOR, NOT A BENCH (PLAN-THE-TESTS phase 7): the deterministic layer decides only what it can read, so on the labelled intent set (bench/intent/cases.json)
    /// every message it DECIDES must be decided right (precision 100 %), and it must keep deciding at least the share it decides today (144 of 164 = 87.8 %; the floor is 87.5 %). A rule that
    /// starts guessing, or a lexicon edit that makes it abstain on messages it used to read, fails here. The bench above prints the detail.
    #[test]
    fn intent_cases_are_decided_right_and_not_fewer() {
        let cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../bench/intent/cases.json")).unwrap()).unwrap();
        let mk_pending = |x: &str| -> Option<Pending> { match x { "approval" => Some(Pending { run: 7, awaiting: Awaiting::Approval, goal: "g".into(), questions: vec![] }),
            "answers" => Some(Pending { run: 7, awaiting: Awaiting::Answers, goal: "g".into(), questions: vec![("q1".into(), vec!["Main App component".into(), "A new component".into()])] }), _ => None } };
        let (mut decided, mut wrong) = (0usize, vec![]);
        for c in &cases {
            let (t, x, want) = (c["t"].as_str().unwrap(), c["x"].as_str().unwrap(), c["k"].as_str().unwrap());
            let ctx = Ctx { recent_run: matches!(x, "run" | "running"), running: x == "running", proposal: x == "proposal" };
            if let Some(k) = resolve_closed(t, mk_pending(x).as_ref()).map(|i| i.kind).or_else(|| classify_open(t, ctx)) {
                decided += 1;
                if k.key() != want { wrong.push(format!("{t:?} [{x}] wanted {want}, got {}", k.key())); }
            }
        }
        assert!(wrong.is_empty(), "decided WRONG (the layer must abstain rather than guess):\n{}", wrong.join("\n"));
        assert!(cases.len() >= 164, "the labelled set must not shrink ({} cases)", cases.len());
        assert!(decided * 1000 >= cases.len() * 875, "the layer decides {decided}/{} - it decided 144/164 (87.8 %) when this floor was set; raise the floor when it rises, never lower it", cases.len());
    }

    /// THE INDEPENDENT CHECK: the operator's own recorded asks (goal-runs.json - each one really became a goal run, so each is a TASK by the only ground truth there is:
    /// what they meant). The rules were written while looking at bench/intent/cases.json, so that set flatters them; this one they never saw.
    #[test]
    #[ignore]
    fn intent_real_asks() {
        let path = std::env::var("GOAL_RUNS").unwrap_or_else(|_| format!("{}/.antiloki/goal-runs.json", std::env::var("HOME").unwrap()));
        let runs: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|_| "[]".into())).unwrap_or_default();
        let mut c: std::collections::BTreeMap<&str, usize> = Default::default(); let mut odd: Vec<String> = vec![];
        for r in &runs {
            let g = r["goal"].as_str().unwrap_or(""); if g.trim().is_empty() || g.len() > 400 { continue }   // a benchmark spec is not a chat message
            let k = match classify_open(g, Ctx::default()) { Some(Kind::Task) => "task", Some(Kind::Question) => "QUESTION", Some(Kind::Chat) => "CHAT", Some(_) => "other", None => "deferred" };
            *c.entry(k).or_default() += 1; if k == "QUESTION" || k == "CHAT" { odd.push(format!("{k}: {g:?}")); }
        }
        println!("\n== REAL ASKS ({} chat-sized of {} runs): {:?}", c.values().sum::<usize>(), runs.len(), c);
        for o in odd.iter().take(20) { println!("   {o}"); }
    }

    #[test]
    fn a_bound_model_is_used_only_when_it_is_named_and_allowed() {
        assert_eq!(choose_low_model(Some(""), Some("composer-2.5"), |_| false).as_deref(), Some("composer-2.5"), "an empty binding is not a model");
        assert_eq!(choose_low_model(Some("haiku"), Some("composer-2.5"), |m| m == "haiku").as_deref(), Some("composer-2.5"), "a refused one gives way to the seat's own rung");
        assert_eq!(choose_low_model(Some("haiku"), Some("composer-2.5"), |_| false).as_deref(), Some("haiku"));
        assert_eq!(choose_low_model(None, Some("x"), |_| true), None); assert_eq!(choose_low_model(None, None, |_| false), None);
    }
    /// the live retest: "the buttons look flat" -> three proposals, all about the buttons -> a bare "yes" asked "which one?"
    #[test]
    fn a_bare_yes_takes_the_first_when_the_proposals_are_variants_of_one_piece_of_work() {
        let buttons = vec![
            "Give primary buttons (Add to cart, Checkout, play/pause, and HUD actions) more depth in `src/App.css` - subtle vertical gradients, soft inner highlight, resting neon glow, and clearer pressed/active states".to_string(),
            "Style cart quantity stepper buttons with a raised circular treatment (background, border, shadow) that matches the gamer neon UI".to_string(),
            "Add a shared button surface pattern in `src/index.css` (tokens plus utility classes) and apply it to interactive buttons across the page".to_string()];
        assert!(proposals_are_variants(&buttons));
        assert_eq!(pick_proposal("yes", &buttons), Some(Pick::First(buttons[0].clone(), 3)));
        assert_eq!(pick_proposal("ok", &buttons), Some(Pick::First(buttons[0].clone(), 3)));
        assert_eq!(pick_proposal("2", &buttons), Some(Pick::One(buttons[1].clone())), "a number still picks");
        let different = vec!["Restore the dark theme toggle in the header".to_string(), "add a sort dropdown for the products".to_string()];
        assert!(!proposals_are_variants(&different));
        assert_eq!(pick_proposal("yes", &different), Some(Pick::Ask), "different features: still asks");
        assert!(!proposals_are_variants(&[]) && !proposals_are_variants(&["only one proposal here".to_string()]));
    }
    #[test]
    fn the_chats_proposals_are_read_back_as_data_and_a_yes_or_a_number_picks_one_without_a_model() {
        let said = "The tag filters are done.\n▸ proposal: Restore the dark theme toggle in the header\n- **Proposal 2:** add a sort dropdown for the products\n`▸ proposal: x`\nWhich one?";
        let props = proposals_of(said);
        assert_eq!(props, vec!["Restore the dark theme toggle in the header", "add a sort dropdown for the products"], "{props:?}");
        assert!(proposals_of("no such line, but a proposal: in prose").is_empty() || proposals_of("no such line").is_empty());
        // several DIFFERENT features on the table: a bare yes asks WHICH (never guesses); a number or an ordinal picks
        assert_eq!(pick_proposal("yes", &props), Some(Pick::Ask)); assert_eq!(pick_proposal("👍", &props), Some(Pick::Ask)); assert_eq!(pick_proposal("sim", &props), Some(Pick::Ask));
        assert_eq!(pick_proposal("2", &props), Some(Pick::One(props[1].clone()))); assert_eq!(pick_proposal("the first one", &props), Some(Pick::One(props[0].clone())));
        assert_eq!(pick_proposal("o segundo", &props), Some(Pick::One(props[1].clone()))); assert_eq!(pick_proposal("option 1", &props), Some(Pick::One(props[0].clone())));
        assert_eq!(pick_proposal("9", &props), None, "there is no ninth"); assert_eq!(pick_proposal("what about the header?", &props), None);
        // one on the table: a yes in any language accepts it
        let one = vec!["Restore the dark theme toggle".to_string()];
        for y in ["yes", "Yes!", "ok", "sim", "vale", "oui", "ja", "👍", "go"] { assert_eq!(pick_proposal(y, &one), Some(Pick::One(one[0].clone())), "{y}"); }
        assert_eq!(pick_proposal("no", &one), None); assert_eq!(pick_proposal("yes", &[]), None);
        // the log: only the newest assistant turn with no operator turn after it counts
        let log = [r#"{"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"▸ proposal: do A"}]}}}"#, r#"{"ev":{"type":"antiloki","subtype":"user","text":"yes"}}"#];
        assert_eq!(last_assistant(log[..1].iter().copied()).as_deref(), Some("▸ proposal: do A")); assert_eq!(last_assistant(log.iter().copied()), None);
        assert!(proposal_pending(log[..1].iter().copied()) && !proposal_pending(log.iter().copied()));
        // …but the "which one?" note the layer itself wrote carries them past the operator's yes, so the number that follows still finds them
        let asked = [log[0], log[1], r#"{"ev":{"type":"antiloki","subtype":"note","text":"▸ which one? say the number\n▸ proposal 1: do A\n▸ proposal 2: do B"}}"#];
        assert_eq!(proposals_of(&last_assistant(asked.iter().copied()).unwrap()), vec!["do A", "do B"]);
        assert_eq!(pick_proposal("2", &proposals_of(&last_assistant(asked.iter().copied()).unwrap())), Some(Pick::One("do B".into())));
    }

    /// the deterministic classifier, a few of the 110 in bench/intent/cases.json as a regression guard (the full bench is `intent_bench`)
    #[test]
    fn the_classifier_is_sure_only_where_the_words_point_at_the_answer() {
        let none = Ctx::default(); let run = Ctx { recent_run: true, ..Default::default() };
        for (t, want) in [("add filters", Kind::Task), ("can you add a footer?", Kind::Task), ("In src/products.ts add a fourth product named Teapot with price 12", Kind::Task),
                          ("where is the cart state kept?", Kind::Question), ("is it possible to sort by price?", Kind::Question), ("thanks", Kind::Chat), ("it crashes on submit", Kind::Task),
                          ("what do you suggest?", Kind::Task), ("can we make the grid responsive", Kind::Task)] {
            assert_eq!(classify_open(t, none), Some(want), "{t}");
        }
        // a message that refers back is the voice's, not the verb rules': "implement that" must not be planned blind
        for t in ["go", "do the first one", "implement that", "and the filtering?"] { assert_eq!(classify_open(t, run), None, "{t}"); }
        // a bare noun decides nothing; a status question needs a run to be about
        assert_eq!(classify_open("filters", none), None);
        assert_eq!(classify_open("is it done?", Ctx { running: true, recent_run: true, ..Default::default() }), Some(Kind::Status));
        assert_eq!(classify_open("is it done?", none), Some(Kind::Chat));
        // "in <target> <verb>" is a task; "in which file..." is not swept in
        assert_eq!(classify_open("in which file is the cart kept", none), Some(Kind::Question));
    }

    /// EDGE SWEEP (prints; nothing asserted): `cargo test -p antiloki-serve --lib edge_sweep -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn edge_sweep() {
        let none = Ctx::default(); let run = Ctx { recent_run: true, ..Default::default() };
        println!("\n== classify_open (no context)");
        for t in ["ADD FILTERS", "add   filters", "Add filters.", "add filters!!", "/fix the bug", "!episode add x", "/model sonnet", "@reviewer add tests", "adicione filtros", "corrija o login", "👍", "add filters 🙂",
                  "TypeError: cannot read properties of undefined (reading 'map')", "Error: ENOENT no such file", "how about adding a footer", "why don't we add filters", "what about pagination?", "don't add filters", "cancel the run and add a footer instead",
                  "stop", "no", "test", "testing 123", "make sure the tests pass", "set up ci", "set of products", "show the cart empty state when there are no items", "can you explain the cart?", "please", "can you", "I want to know how sorting works",
                  "fix: typo in header", "update: bump react", "add", "add.", "  ", "…", "a", "yes", "ok", "thanks!", "Thanks!!!", "THANKS", "lgtm", "+1", "sounds good", "do it", "add filters\nand sorting\nand pagination"] {
            println!("   {:?} -> {:?}", t, classify_open(t, none));
        }
        println!("== with a recent run");
        for t in ["go", "Go!", "GO", "go.", "go go go", "yes!", "do it", "do that", "and?", "and the header", "what about the header?", "why?", "ok", "fine", "hmm ok go", "yes please do the first", "add tests for it", "make it faster", "the first one"] { println!("   {:?} -> {:?}", t, classify_open(t, run)); }
        println!("== resolve_closed: approval pending");
        let plan = Pending { run: 1, awaiting: Awaiting::Approval, goal: "g".into(), questions: vec![] };
        for t in ["go", "Go!", "GO.", "yes!", "y", "yep", "go for it", "ok go", "approve it", "lgtm", "looks good", "cancel", "no", "nope", "don't", "don't do it", "no thanks", "stop", "abort", "hold on", "wait", "not yet", "yes but only the header"] { println!("   {:?} -> {:?}", t, resolve_closed(t, Some(&plan)).map(|i| i.kind)); }
        println!("== resolve_closed: two questions, one chip text in both");
        let q = Pending { run: 1, awaiting: Awaiting::Answers, goal: "g".into(), questions: vec![("q1".into(), vec!["Keep it".into(), "Rewrite it".into(), "Yes".into()]), ("q2".into(), vec!["Keep it".into(), "Drop it".into(), "No".into()])] };
        for t in ["Keep it", "keep it.", "Rewrite it", "Drop it", "Yes", "No", "go", "cancel", "keep it and drop it"] { println!("   {:?} -> {:?}", t, resolve_closed(t, Some(&q)).map(|i| (i.kind, i.answer))); }
        println!("== pending on BOTH kinds: newest wins");
    }

    /// THE LAYER IS LANGUAGE-AGNOSTIC (operator 2026-09-29): the same facts in Portuguese, Spanish, French, German, Italian; structure without words; a language with no
    /// pack is left undecided (the voice reads it), never mis-decided.
    #[test]
    fn the_layer_reads_the_operators_language_and_decides_nothing_it_cannot_read() {
        let none = Ctx::default(); let run = Ctx { recent_run: true, ..Default::default() };
        for (t, want) in [("adicione filtros de produto", Kind::Task), ("corrija o login", Kind::Task), ("você pode adicionar um rodapé?", Kind::Task), ("no carrinho, adicione um botão de remover", Kind::Task),
                          ("onde fica o estado do carrinho?", Kind::Question), ("como funciona o frete", Kind::Question), ("obrigado", Kind::Chat), ("valeu!", Kind::Chat),
                          ("agrega un filtro por precio", Kind::Task), ("¿dónde se guarda el carrito?", Kind::Question), ("gracias", Kind::Chat),
                          ("ajoute un pied de page", Kind::Task), ("où est l'état du panier ?", Kind::Question), ("merci", Kind::Chat),
                          ("füge einen Footer hinzu", Kind::Task), ("erstelle eine Suchleiste", Kind::Task), ("wo liegt der Warenkorb?", Kind::Question), ("danke", Kind::Chat),
                          ("aggiungi un filtro", Kind::Task), ("dove sta il carrello?", Kind::Question), ("grazie", Kind::Chat),
                          ("フッターはどこですか？", Kind::Question), ("👍", Kind::Chat), ("🙂🙂", Kind::Chat)] {
            assert_eq!(classify_open(t, none), Some(want), "{t}");
        }
        // structure needs no words: a slash command, a bang line, a mention, a heading, a paste are never classified as prose
        for t in ["/fix the bug", "/model sonnet", "!episode add x", "@reviewer add tests", "# add filters", "$ npm test", &"add a line\n".repeat(8)] { assert_eq!(classify_open(t, none), None, "{t:?}"); }
        // a language with no pack (Russian, Polish here) is undecided - the voice reads it - and never wrongly a task
        for t in ["добавь фильтры", "dodaj filtry", "проверь корзину", "追加してください フッター"] { assert_eq!(classify_open(t, none), None, "{t}"); }
        // referring back works in the operator's language too
        assert_eq!(classify_open("faz isso", run), None); assert_eq!(classify_open("e o cabeçalho?", run), None); assert_eq!(classify_open("sí, hazlo", run), None);
        // suggestions are for the model
        assert_eq!(classify_open("que tal um modo escuro", none), None); assert_eq!(classify_open("what about pagination", none), None);
        // closed replies: any language, an emoji, a chip in any script, and a chip that several questions offer answers them all
        let plan = pend(Awaiting::Approval);
        for (t, want) in [("pode ir", Kind::Approve), ("sim", Kind::Approve), ("Vamos", Kind::Approve), ("adelante", Kind::Approve), ("d'accord", Kind::Approve), ("mach das", Kind::Approve), ("va bene", Kind::Approve), ("👍", Kind::Approve), ("✅", Kind::Approve),
                          ("cancela", Kind::Cancel), ("não", Kind::Cancel), ("olvídalo", Kind::Cancel), ("annule", Kind::Cancel), ("abbrechen", Kind::Cancel), ("annulla", Kind::Cancel), ("🛑", Kind::Cancel), ("👎", Kind::Cancel)] {
            assert_eq!(resolve_closed(t, Some(&plan)).map(|i| i.kind), Some(want), "{t}");
        }
        assert!(resolve_closed("🙂", Some(&plan)).is_none() && resolve_closed("добавь фильтры", Some(&plan)).is_none());
        let jp = Pending { run: 3, awaiting: Awaiting::Answers, goal: "g".into(), questions: vec![("q1".into(), vec!["そのまま残す".into(), "書き直す".into()]), ("q2".into(), vec!["そのまま残す".into(), "Мой вариант".into()])] };
        assert_eq!(resolve_closed("そのまま残す", Some(&jp)).map(|i| (i.kind, i.answer)), Some((Kind::Answer, "そのまま残す".into())));
        assert_eq!(resolve_closed("мой вариант", Some(&jp)).map(|i| (i.kind, i.answer)), Some((Kind::Answer, "Мой вариант".into())));
    }

    #[test]
    fn closed_replies_are_state_transitions_resolved_without_a_model() {
        let plan = pend(Awaiting::Approval); let q = pend(Awaiting::Answers);
        assert_eq!(resolve_closed("go", Some(&plan)).map(|i| (i.kind, i.run)), Some((Kind::Approve, Some(7))));
        assert_eq!(resolve_closed("  Yes, please! ", Some(&plan)).map(|i| i.kind), Some(Kind::Approve));
        assert_eq!(resolve_closed("cancel", Some(&plan)).map(|i| i.kind), Some(Kind::Cancel));
        // with questions waiting, "go" takes the defaults; a chip's exact text answers; anything else is not closed
        assert_eq!(resolve_closed("go", Some(&q)).map(|i| (i.kind, i.answer)), Some((Kind::Answer, String::new())));
        let chip = resolve_closed("add product discovery and filtering", Some(&q)).expect("a chip");
        assert_eq!((chip.kind, chip.answer.as_str()), (Kind::Answer, "Add product discovery and filtering"));
        assert!(resolve_closed("actually make it faster too", Some(&q)).is_none());
        // a chip is an answer at ANY length: seven words, found in the browser (it used to start a new run)
        let long = Pending { run: 3, awaiting: Awaiting::Answers, goal: "g".into(), questions: vec![("q1".into(), vec!["create `src/checkout.ts`".into(), "change nothing until I confirm the path".into(), "skip that file and do the rest of the ask".into()])] };
        for chip in ["change nothing until I confirm the path", "skip that file and do the rest of the ask", "Create src/checkout.ts"] {
            let i = resolve_closed(chip, Some(&long)).expect(chip); assert_eq!((i.kind, i.run), (Kind::Answer, Some(3)), "{chip}");
        }
        assert!(resolve_closed("change nothing until I confirm the path is fixed and also rename the file", Some(&long)).is_none(), "a sentence that merely contains a chip is language");
        // no pending run: nothing to transition
        assert!(resolve_closed("go", None).is_none());
        // long messages are language, never closed
        assert!(resolve_closed("yes but only for the products page and not the cart drawer", Some(&plan)).is_none());
    }

    fn run(id: i64, pane: i64, status: RunStatus, goal: &str) -> GoalRun {
        let mut r: GoalRun = serde_json::from_value(json!({ "id": id, "parent": "p", "goal": goal, "engine": "claude-code", "cwd": ".", "write": false, "concurrency": 1, "status": "done", "created": 1_000_000, "tasks": [], "blackboard": [], "events": [], "delivered": false, "router": true, "min": false, "plan_args": { "n": 4, "router": true, "auto": false, "always": false }, "stages": [], "qa_double": false, "qa_review_wanted": false, "qa_baseline_pending": false, "mid_task": false })).expect("a run");
        r.status = status; r.pane_id = pane; r
    }

    #[test]
    fn the_digest_carries_the_question_the_answer_and_the_report_and_only_this_panes_recent_runs() {
        let mut r = run(258, 4, RunStatus::Done, "what do you suggest?");
        r.questions = vec![crate::goals::Question { id: "q1".into(), text: "Which outcome should the next change prioritize?".into(), options: vec![], default: "Improve UX".into(), certainty: Default::default(), answer: Some("Add product discovery and filtering".into()), kind: String::new() }];
        r.report = Some("1. Persist the cart\n2. Accessibility\n3. Filtering, search".into());
        let other = run(9, 5, RunStatus::Done, "another pane's work");
        let old = run(3, 4, RunStatus::Done, "an hour ago");
        let d = digest(&[old, r.clone(), other], 4, 1_000_000 + 60_000);
        assert!(d.contains("run #258") && d.contains("the operator answered: Add product discovery and filtering") && d.contains("3. Filtering, search"), "{d}");
        assert!(!d.contains("another pane") , "{d}");
        assert!(digest(&[r], 4, 1_000_000 + 40 * 60_000).is_empty(), "a run older than half an hour is not in the conversation any more");
        assert!(digest(&[], 4, 0).is_empty());
    }

    /// found in the operator's conversation: "add filters" -> the chat proposed "say go: product filters by tag (+ search)" -> "go" had no run to refer to
    #[test]
    fn a_proposal_the_chat_just_made_gives_a_bare_go_something_to_refer_to() {
        let ask = r#"{"seq":1,"t":1,"ev":{"type":"antiloki","subtype":"user","text":"add filters"}}"#;
        let propose = r#"{"seq":2,"t":2,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"There are no filters yet. If you want that run as a delegated goal, say go: product filters by tag (+ search)."}]}}}"#;
        let plain = r#"{"seq":2,"t":2,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"There are no filters yet; the grid renders every product."}]}}}"#;
        let go = r#"{"seq":3,"t":3,"ev":{"type":"antiloki","subtype":"user","text":"go"}}"#;
        assert!(proposal_pending([ask, propose].join("\n").lines()));
        assert!(!proposal_pending([ask, plain].join("\n").lines()), "an answer that does not invite a go");
        assert!(!proposal_pending([ask, propose, go].join("\n").lines()), "the operator already replied");
        assert!(!proposal_pending("".lines()));
    }

    #[test]
    fn only_a_short_referring_message_after_a_run_goes_to_the_voice() {
        let r = run(1, 4, RunStatus::Done, "x"); let now = 1_000_000 + 1000;
        for t in ["go", "do the first one", "and the filtering?", "implement that", "ok"] { assert!(wants_voice(t, &[r.clone()], 4, now, false), "{t}"); }
        for t in ["add authentication", "add a button", "fix the login"] { assert!(!wants_voice(t, &[r.clone()], 4, now, false), "a new short ask is not a reference: {t}"); }
        assert!(!wants_voice("rename the constant in src/config.ts and update its two callers to match", &[r.clone()], 4, now, false), "a long message is its own ask");
        // the chat itself proposed work and waits for a go: "go" has something to refer to even with no run at all
        assert!(wants_voice("go", &[], 4, now, true) && !wants_voice("add authentication", &[], 4, now, true));
        assert!(!wants_voice("go", &[], 4, now, false), "no run, nothing to refer to");
        assert!(!wants_voice("go", &[r], 9, now, false), "another pane's runs are not this conversation");
    }

    #[test]
    fn a_digest_rides_in_front_of_a_turn_only_when_it_changed() {
        assert!(fresh(901, "run #1 done"));
        assert!(!fresh(901, "run #1 done"), "the session already has it");
        assert!(fresh(901, "run #1 done\n run #2 waiting"), "a run moved");
        assert!(fresh(902, "run #1 done"), "another pane has its own memory");
    }

    #[test]
    fn the_lint_holds_the_intent_to_the_template() {
        let q = pend(Awaiting::Answers); let runs = [7i64];
        // good ones
        assert!(lint_intent(r#"{"kind":"task","run":null,"ask":"Add product discovery and filtering to the store, as the report suggested","answer":"","why":"go after a report"}"#, None, &runs).is_ok());
        assert!(lint_intent(r#"{"kind":"answer","run":7,"ask":"","answer":"Fix accessibility","why":""}"#, Some(&q), &runs).is_ok());
        // bad ones, each with a reason fit to send back
        let e = lint_intent(r#"{"kind":"task","ask":"go"}"#, None, &runs).unwrap_err(); assert!(e[0].contains("at least 3 words"), "{e:?}");
        let e = lint_intent(r#"{"kind":"approve","run":7}"#, None, &runs).unwrap_err(); assert!(e.iter().any(|x| x.contains("nothing is waiting")), "{e:?}");
        let e = lint_intent(r#"{"kind":"cancel","run":3}"#, Some(&q), &runs).unwrap_err(); assert!(e[0].contains("not the run waiting"), "{e:?}");
        let e = lint_intent(r#"{"kind":"nonsense"}"#, None, &runs).unwrap_err(); assert!(e[0].contains("must be one of"), "{e:?}");
        let e = lint_intent(r#"{"kind":"chat","mood":"happy"}"#, None, &runs).unwrap_err(); assert!(e[0].contains("unknown field"), "{e:?}");
        let e = lint_intent("sure! here you go", None, &runs).unwrap_err(); assert!(e[0].contains("no JSON object"), "{e:?}");
        let e = lint_intent(r#"{"kind":"chat","ask":"stray text"}"#, None, &runs).unwrap_err(); assert!(e[0].contains("empty unless"), "{e:?}");
    }

    #[tokio::test]
    async fn a_reply_that_fails_the_lint_is_sent_back_to_be_edited_and_a_stubborn_one_falls_back_to_chat() {
        // first reply is prose, the second (after the re-edit prompt carrying the error) is a good object
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let script = std::sync::Arc::new(std::sync::Mutex::new(vec!["Sure! I will do that.".to_string(), r#"{"kind":"task","run":null,"ask":"Add product discovery and filtering to the store","answer":"","why":"go after the report"}"#.to_string()]));
        let (c2, s2) = (calls.clone(), script.clone());
        let (i, src, seen) = interpret("go", "digest text", None, &[258], move |p| { let (c2, s2) = (c2.clone(), s2.clone()); async move { c2.lock().unwrap().push(p); Ok(s2.lock().unwrap().remove(0)) } }).await;
        assert_eq!((i.kind, src), (Kind::Task, "model, re-edited")); assert!(i.ask.starts_with("Add product discovery"));
        assert!(seen[0].contains("no JSON object"));
        { let c = calls.lock().unwrap(); assert_eq!(c.len(), 2); assert!(!c[0].contains("REJECTED") && c[1].contains("YOUR PREVIOUS REPLY WAS REJECTED") && c[1].contains("Sure! I will do that.") && c[1].contains("no JSON object"), "{}", c[1]); assert!(c[0].contains("digest text") && c[0].contains("\"go\"")); }
        // never lints: three tries, then the ordinary road
        let (i, src, seen) = interpret("go", "", None, &[], |_p| async { Ok("no".to_string()) }).await;
        assert_eq!((i.kind, src), (Kind::Chat, "fallback")); assert_eq!(seen.len(), 3);
        // the model call itself failing is a fallback too, not an error
        let (i, src, _) = interpret("go", "", None, &[], |_p| async { Err::<String, String>("timed out".into()) }).await;
        assert_eq!((i.kind, src), (Kind::Chat, "fallback"));
    }
}
