//! THE DESIGN ROUND - PLAN-THE-DESIGN S5 / D4 (2026-10-01): what the run does with a page that LOOKS wrong, inside the send-back loop QA already has.
//!
//! Operator 2026-10-01: Claude's result looked clearly better than Composer's and nothing in the system could say so. S1 measures a page without a model (`design_lint`), S2 compares two pages with
//! one (`looks`). This is the part that ACTS: after the behavioural QA has passed and before delivery, ONE critique of the page by a seat with eyes, handed to the builder as the next send-back brief.
//!
//!   * who critiques: the cheapest vision-MEASURED seat (`looks::VISION_SEATS`) that is on, signed in and not out of quota - not necessarily the builder (`critic_seat_with`);
//!   * what it is given: the screenshots, the design lint block, the rubric (`design.critique`, the rubric spliced from `design.rubric` like the judge's);
//!   * what comes back: at most five concrete fixes as JSON (`parse_fixes`) - each one numbered, one line, quoting a measured value;
//!   * what BLOCKS delivery: only the D1 failures the operator turned into rules (`DesignLint::blockers`: body text under 3:1, the page sticking out sideways at 1280 or 390 px, clipped text). They are
//!     facts, found without a model, and they go back to the builder as hard feedback exactly like an unmet claim, inside the same `qa_rounds` cap;
//!   * what NEVER blocks: taste. The critic's fixes go back ONCE (one design round per run, `decide`), as polish in the same brief, and whatever is not sent (the cap is spent, send-backs are off) is
//!     written on the report as "design notes". A page that is still plain after the round is delivered plain, with the notes.
//! Calibration (D3) is NOT needed for any of this: a blocker is arithmetic, a fix is advice. It IS needed to RANK seats by looks (`looks::looks_may_route`) and nothing here writes a look.
use crate::cli::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// At most this many fixes reach the builder: a longer list is a rewrite, and the builder does not prioritise.
pub const MAX_FIXES: usize = 5;
/// One design round per run (plan D4). It is also bounded by the run's `qa_rounds` cap: it never adds a round the cap does not allow.
pub const MAX_DESIGN_ROUNDS: u32 = 1;
/// The most shots the critic is shown (each is ~1 370 image tokens): the last ones, where the page is in its final state.
pub const MAX_SHOTS: usize = 3;

// ------------------------------------------------------------------------------------------------------------------------------ the setting
/// `settings.qa.design`: "auto" (the default - on for frontend and design work, off for everything else), "on", "off".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode { Auto, On, Off }
impl Mode {
    pub fn parse(v: &Value) -> Option<Mode> {
        match v {
            Value::Bool(true) => Some(Mode::On), Value::Bool(false) => Some(Mode::Off),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() { "auto" | "" => Some(Mode::Auto), "on" | "true" | "yes" => Some(Mode::On), "off" | "false" | "no" => Some(Mode::Off), _ => None },
            _ => None,
        }
    }
    pub fn key(self) -> &'static str { match self { Mode::Auto => "auto", Mode::On => "on", Mode::Off => "off" } }
}
/// Is the design round wanted for work in this domain? `auto` = frontend and design only (a backend change that happens to touch a stylesheet is not a design task). A page written as plain `.html` is a
/// design task too even though the domain classifier has no word for it (it files `index.html` as unknown): HTML counts when the files do not say backend. Pure - tested.
pub fn enabled(mode: Mode, domain: crate::roles::Domain, files: &[String]) -> bool {
    use crate::roles::Domain::*;
    let html = files.iter().any(|f| { let f = f.to_ascii_lowercase(); f.ends_with(".html") || f.ends_with(".htm") });
    match mode { Mode::Off => false, Mode::On => true, Mode::Auto => matches!(domain, Frontend | Design) || (html && matches!(domain, Unknown | Mixed)) }
}
pub fn settings_path() -> std::path::PathBuf { crate::eta::History::path().with_file_name("settings-qa.json") }
/// The mode written at `path` (`{"design": "auto"|"on"|"off"}`); anything unreadable is `auto`.
pub fn mode_at(path: &std::path::Path) -> Mode {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.get("design").and_then(Mode::parse)).unwrap_or(Mode::Auto)
}
/// The mode in force: `ANTILOKI_QA_DESIGN=auto|on|off` wins when it is one of those (a bench, a journey, a machine that must never run it), else the settings file, else auto.
pub fn mode() -> Mode { std::env::var("ANTILOKI_QA_DESIGN").ok().and_then(|v| Mode::parse(&json!(v))).unwrap_or_else(|| mode_at(&settings_path())) }
pub fn set_mode_at(path: &std::path::Path, m: Mode) -> Result<(), String> {
    if let Some(d) = path.parent() { let _ = std::fs::create_dir_all(d); }
    let mut v: Value = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}));
    v["design"] = json!(m.key());
    std::fs::write(path, serde_json::to_string_pretty(&v).unwrap_or_default()).map_err(|e| e.to_string())
}
/// GET /api/v1/settings/qa -> { design: "auto"|"on"|"off", default: "auto" }
pub async fn http_get() -> axum::Json<Value> { axum::Json(json!({ "design": mode().key(), "default": "auto", "rule": "auto = on for frontend and design asks only" })) }
/// POST /api/v1/settings/qa { design: "auto"|"on"|"off"|true|false }
pub async fn http_set(axum::Json(body): axum::Json<Value>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match body.get("design").and_then(Mode::parse) {
        Some(m) => match set_mode_at(&settings_path(), m) { Ok(()) => axum::Json(json!({ "ok": true, "design": m.key() })).into_response(), Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response() },
        None => (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({ "ok": false, "error": "design must be auto, on or off" }))).into_response(),
    }
}

// ------------------------------------------------------------------------------------------------------------------------------ the state on the run
/// One concrete fix, as the critic wrote it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fix { pub axis: String, pub r#where: String, pub problem: String, pub fix: String, pub evidence: String }
impl Fix {
    /// The brief line: `#2 [spacing] toolbar: it touches the grid -> increase the gap to 24px (lint spacing_scale: 6px)`. One line, bounded.
    pub fn line(&self, n: usize) -> String {
        let clipc = |s: &str, k: usize| -> String { let t: String = s.split_whitespace().collect::<Vec<_>>().join(" "); if t.chars().count() <= k { t } else { format!("{}…", t.chars().take(k).collect::<String>()) } };
        let mut s = format!("#{n} [{}] {}: {} -> {}", if self.axis.is_empty() { "design" } else { &self.axis }, clipc(&self.r#where, 80), clipc(&self.problem, 160), clipc(&self.fix, 200));
        if !self.evidence.trim().is_empty() { s.push_str(&format!(" (seen: {})", clipc(&self.evidence, 120))); }
        s
    }
}

/// IS A POLISH FIX ABOUT WHAT THIS CHANGE TOUCHED? (operator 2026-10-09, live test: "the cart looks ugly" was sent back to resize `button.chapter-pill` by 1 px, "add dark mode" to resize a button, and one such fix
/// dropped the page's lint from 0.79 to 0.58 and caused a second send-back - 6-10 of 14 minutes were design rounds on things the ask never named.) A fix is in scope when the element it names (the class, id or
/// custom tag in its `where`) appears on a line the change ADDED, or when its `where` shares a word with the ask; when every changed file is NEW (`all_new`) everything is in scope. A `where` that names nothing identifiable stays in scope: not knowing is not a reason to drop it.
/// Returns `(in scope, out of scope)`, order kept. Pure - tested.
pub fn partition_in_scope(fixes: Vec<Fix>, added_lines: &str, ask: &str, all_new: bool) -> (Vec<Fix>, Vec<Fix>) {
    // a page this run CREATED has nothing that pre-dates it: every fix is about what the change made
    if all_new { return (fixes, vec![]) }
    const GENERIC: &[&str] = &["div", "span", "button", "section", "header", "footer", "main", "nav", "aside", "article", "ul", "li", "ol", "p", "a", "body", "html", "svg", "img", "input", "form", "label", "select", "textarea", "h1", "h2", "h3", "page", "the", "and", "for", "with", "all", "root", "app", "panel", "page-wide", "global", "layout", "element", "elements", "text", "buttons", "controls"];
    const STOP: &[&str] = &["make", "the", "more", "add", "change", "like", "that", "this", "with", "from", "into", "look", "looks", "design", "better", "need", "want", "please", "now", "again"];
    let added = added_lines.to_lowercase(); let ask_l = ask.to_lowercase();
    let ask_words: Vec<&str> = ask_l.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 4 && !STOP.contains(w)).collect();
    let mut inn = vec![]; let mut out = vec![];
    for f in fixes {
        let w = f.r#where.to_lowercase();
        let toks: Vec<&str> = w.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')).filter(|t| t.len() >= 3 && !GENERIC.contains(t)).collect();
        let named = !toks.is_empty();
        let touched = toks.iter().any(|t| added.contains(t)) || toks.iter().any(|t| ask_words.iter().any(|a| t.contains(a) || a.contains(t)));
        if !named || touched { inn.push(f) } else { out.push(f) }
    }
    (inn, out)
}

/// Everything the design round knows about a run. Lives on the `GoalRun` (`design`, serde default) so a restarted serve still knows what was sent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DesignState {
    /// the latest capture's `DesignLint::to_json()` (checks, score)
    pub lint: Option<Value>,
    /// the lint block the critic reads (`DesignLint::prompt_block`)
    pub lint_block: String,
    pub score: Option<f64>,
    /// the score of the FIRST capture of the run: the "before" the round is judged against
    pub score_first: Option<f64>,
    /// the BEST score any capture of the run reached: the report says so when the delivered page measures lower (a later send-back made it worse)
    #[serde(default)]
    pub score_best: Option<f64>,
    /// the latest capture's desktop screenshots (absolute paths)
    pub shots: Vec<String>,
    /// the rule failures of the latest capture, as brief lines - hard feedback
    pub blockers: Vec<String>,
    /// the critic's fixes; `sent` = already in a builder's brief
    pub fixes: Vec<Fix>,
    pub sent: bool,
    /// design rounds spent (the critic ran and produced fixes)
    pub rounds: u32,
    /// the critic's seat ("cursor/composer-2.5") and what it cost / took
    pub seat: String,
    pub usd: f64,
    pub ms: i64,
    /// the report already carries the design notes
    pub reported: bool,
    /// the page's files as the critic saw them (copied before the builder rewrites them), so "before" can be photographed again under the same camera as "after"
    pub before_dir: String,
    /// the critic's fixes about something this change did NOT touch (a pre-existing 31px button the ask never mentioned): written on the report, never sent back (2026-10-09)
    #[serde(default)]
    pub out_of_scope: Vec<Fix>,
}
impl DesignState {
    /// A fresh capture: the lint, the pictures, the blockers. The first capture of the run fixes `score_first`.
    pub fn observe(&mut self, lint: &crate::design_lint::DesignLint, shots: Vec<String>) {
        self.lint = Some(lint.to_json()); self.lint_block = lint.prompt_block(); self.score = lint.score; if let Some(s) = self.score { if self.score_best.map_or(true, |b| s > b) { self.score_best = Some(s); } } self.shots = shots; self.blockers = lint.blockers();
        if self.score_first.is_none() { self.score_first = lint.score; }
    }
    /// The page could not be measured this time: what was blocking last time is not known to still be true, and nothing is invented.
    pub fn unmeasured(&mut self) { self.lint = None; self.lint_block.clear(); self.score = None; self.shots.clear(); self.blockers.clear(); }
    /// The fixes not yet in a brief, as numbered lines.
    pub fn unsent_lines(&self) -> Vec<String> { if self.sent { vec![] } else { self.fixes.iter().enumerate().map(|(i, f)| f.line(i + 1)).collect() } }
    /// The sentence the report and the log carry: what the round did and what the lint said before and after.
    pub fn summary(&self) -> String {
        let sc = |s: Option<f64>| s.map(|x| format!("{x:.2}")).unwrap_or_else(|| "n/a".into());
        format!("design round: {} fix(es) from {}, +${:.4}, design lint {} -> {}", self.fixes.len(), if self.seat.is_empty() { "no seat" } else { &self.seat }, self.usd, sc(self.score_first), sc(self.score))
    }
    /// The block appended to a delivered report: the notes and the before/after. `None` when the round never ran.
    pub fn report_block(&self) -> Option<String> {
        if self.rounds == 0 && self.fixes.is_empty() { return None }
        let mut s = format!("\n\nDESIGN NOTES ({})", self.summary());
        for (i, f) in self.fixes.iter().enumerate() { s.push_str(&format!("\n- {}{}", f.line(i + 1), if self.sent { "" } else { "  [not applied]" })); }
        if !self.out_of_scope.is_empty() {
            s.push_str("\n- also noticed, outside this ask - NOT changed:");
            for f in &self.out_of_scope { s.push_str(&format!("\n  - {}", f.line(0).trim_start_matches("#0 "))); }
        }
        if !self.blockers.is_empty() { s.push_str("\n- still failing a design rule:"); for b in &self.blockers { s.push_str(&format!("\n  - {b}")); } }
        // A LATER SEND-BACK CAN UNDO THE POLISH (live check 2026-10-09: 1.00 after the design round, 0.89 after a send-back for the report's wording; the round was spent): say so, do not hide it
        if let (Some(best), Some(now)) = (self.score_best, self.score) { if best - now >= 0.02 { s.push_str(&format!("\n- the page measured {best:.2} at its best and {now:.2} when delivered: a later send-back made the design worse")); } }
        Some(s)
    }
}

// ------------------------------------------------------------------------------------------------------------------------------ whether to run
/// Everything the decision depends on, as plain values.
#[derive(Clone, Copy, Debug)]
pub struct Case { pub enabled: bool, pub measured: bool, pub gate_unmet: usize, pub rounds_spent: u32, pub qa_rounds: u32, pub max_rounds: u32, pub seat_available: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision { Critique, Skip(&'static str) }
/// May the critic run NOW? In this order: the setting; a measured page; the behaviour is right first (a design round on a page whose claims are unmet is wasted: the builder is going back anyway); the one
/// design round is not spent; a send-back is possible within the QA cap (a critique nobody can act on is money for nothing - it would only be a note); a seat with eyes is available. Pure - tested.
pub fn decide(c: &Case) -> Decision {
    if !c.enabled { return Decision::Skip("the design round is off for this kind of work") }
    if !c.measured { return Decision::Skip("the page could not be measured") }
    if c.gate_unmet > 0 { return Decision::Skip("the behaviour is not right yet - design comes after QA passes") }
    if c.rounds_spent >= MAX_DESIGN_ROUNDS { return Decision::Skip("the design round of this run is spent") }
    if c.max_rounds == 0 || c.qa_rounds >= c.max_rounds { return Decision::Skip("no send-back is left in the QA cap") }
    if !c.seat_available { return Decision::Skip("no seat with eyes is available (off, signed out or out of quota)") }
    Decision::Critique
}

/// The critic: of the vision-MEASURED seats that can take a call today, the cheapest for this job (`price` = dollars for the job at that seat), ties in `QA_GATE_ORDER`'s order. The measured vision quality of
/// the candidates is the same (they all read the screenshot 8 of 8 in bench/vision) so the tolerance is met by every one of them and the price decides. `None` when no seat qualifies. Pure over availability and price.
pub fn critic_seat_with(ok: impl Fn(Engine, Option<&str>) -> bool, price: impl Fn(Engine, &str) -> f64) -> Option<(Engine, &'static str)> {
    let mut best: Option<((Engine, &'static str), f64)> = None;
    for (e, m) in crate::cli::QA_GATE_ORDER.iter().copied().filter(|s| crate::looks::VISION_SEATS.contains(s) && ok(s.0, Some(s.1))) {
        let p = price(e, m);
        if best.as_ref().map(|(_, bp)| p < *bp).unwrap_or(true) { best = Some(((e, m), p)); }
    }
    best.map(|b| b.0)
}
/// The estimated dollars of one critique at a seat: `images` screenshots plus ~3k text in, ~700 out, at the seat's list price (the same table the claim check is priced from).
pub fn critique_price(engine: Engine, model: &str, images: usize) -> f64 {
    let (pin, pout) = crate::cli::prices_of(engine, Some(model));
    ((images as f64 * 1_370.0 + 3_000.0) * pin + 700.0 * pout) / 1e6
}
/// `ANTILOKI_DESIGN_CRITIC=sonnet` (or `claude-code:sonnet`) pins the critic to ONE of the vision-measured seats - for a bench, a journey with a scripted seat, or an operator who wants a particular pair of eyes. The pin never
/// widens the list: a seat that has no measured vision, or cannot take a call, is still not asked.
pub fn pinned(pin: Option<&str>, e: Engine, m: &str) -> bool { pin.map(str::trim).filter(|p| !p.is_empty()).map(|p| p == m || p == format!("{}:{m}", e.key())).unwrap_or(true) }
pub fn critic_seat(images: usize) -> Option<(Engine, &'static str)> {
    let pin = std::env::var("ANTILOKI_DESIGN_CRITIC").ok();
    critic_seat_with(|e, m| m.map(|m| pinned(pin.as_deref(), e, m)).unwrap_or(true) && crate::cli::seat_ok(e, m), |e, m| critique_price(e, m, images))
}

// ------------------------------------------------------------------------------------------------------------------------------ the critic's answer
/// The fixes in the critic's answer: the LAST JSON object with a `fixes` array (a critic that thinks aloud first is read by its answer). At most `MAX_FIXES`, in the order given ("most visible first"); a fix
/// with no `fix` text is dropped; the same fix twice is kept once. An empty list is an answer ("the page already looks good"), not an error. No readable object -> `Err`. Pure - tested.
pub fn parse_fixes(text: &str) -> Result<Vec<Fix>, String> {
    let mut found: Option<Vec<Value>> = None;
    for (i, _) in text.match_indices('{') {
        let Some(Ok(Value::Object(o))) = serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>().next() else { continue };
        if let Some(Value::Array(a)) = o.get("fixes") { found = Some(a.clone()); }
    }
    let Some(arr) = found else { return Err(format!("no JSON with a `fixes` list in the answer: {}", text.trim().chars().take(160).collect::<String>())) };
    let get = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(|s| s.trim().to_string()).unwrap_or_default();
    let mut out: Vec<Fix> = vec![];
    for v in arr {
        // a bare string is a fix with no frame
        let f = if let Some(s) = v.as_str() { Fix { fix: s.trim().to_string(), ..Default::default() } }
            else { Fix { axis: get(&v, "axis").to_ascii_lowercase(), r#where: get(&v, "where"), problem: get(&v, "problem"), fix: get(&v, "fix"), evidence: get(&v, "evidence") } };
        if f.fix.chars().count() < 8 || out.iter().any(|x| x.fix == f.fix) { continue }
        out.push(f);
        if out.len() == MAX_FIXES { break }
    }
    Ok(out)
}

/// Copies the changed files of the run into `dest` as they are NOW - the page the critic is about to judge - and says how many. Relative paths inside the box only; a file over 1 MB is not a page. Never fatal.
pub fn keep_before(cwd: &str, files: &[String], dest: &std::path::Path) -> usize {
    use std::path::{Component, Path};
    let mut n = 0;
    for f in files {
        let rel = Path::new(f);
        if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) { continue }
        let src = Path::new(cwd).join(rel);
        if !std::fs::metadata(&src).map(|m| m.is_file() && m.len() <= 1_000_000).unwrap_or(false) { continue }
        let to = dest.join(rel);
        if let Some(d) = to.parent() { if std::fs::create_dir_all(d).is_err() { continue } }
        if std::fs::copy(&src, &to).is_ok() { n += 1; }
    }
    n
}

/// The shots the critic is shown: the last `MAX_SHOTS`, in order. Pure - tested.
pub fn pick_shots(shots: &[String]) -> Vec<String> { shots[shots.len().saturating_sub(MAX_SHOTS)..].to_vec() }

/// The `design.critique` prompt: the ask, the rubric (spliced from the registered `design.rubric`), the pictures' paths and the lint block.
pub fn critique_prompt(ask: &str, shots: &[String], lint_block: &str) -> String {
    let listed = shots.iter().enumerate().map(|(i, p)| format!("  screenshot {} (desktop 1280x800): {p}", i + 1)).collect::<Vec<_>>().join("\n");
    let mut v = serde_json::Map::new();
    v.insert("ask".into(), json!(ask.chars().take(2000).collect::<String>()));
    v.insert("rubric".into(), json!(crate::prompts::design_rubric()));
    v.insert("shots".into(), json!(listed));
    v.insert("lint".into(), json!(lint_block));
    crate::prompts::render_base("design.critique", &v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roles::Domain;

    fn case() -> Case { Case { enabled: true, measured: true, gate_unmet: 0, rounds_spent: 0, qa_rounds: 0, max_rounds: 2, seat_available: true } }

    #[test]
    fn the_setting_is_on_for_frontend_and_design_asks_only_unless_the_operator_says_otherwise() {
        for (d, auto) in [(Domain::Frontend, true), (Domain::Design, true), (Domain::Backend, false), (Domain::Tests, false), (Domain::Docs, false), (Domain::Mixed, false), (Domain::Unknown, false)] {
            assert_eq!(enabled(Mode::Auto, d, &[]), auto, "{d:?} under auto");
            assert!(enabled(Mode::On, d, &[]), "{d:?} under on");
            assert!(!enabled(Mode::Off, d, &[]), "{d:?} under off");
        }
        // a plain HTML page is a design task although the domain classifier files index.html as unknown - but not when the files say backend
        let page = vec!["index.html".to_string()];
        assert!(enabled(Mode::Auto, crate::roles::domain_of(&page), &page), "index.html alone");
        assert!(!enabled(Mode::Auto, Domain::Backend, &page), "a backend change that ships one html file is not a design task");
        assert!(!enabled(Mode::Auto, crate::roles::domain_of(&["notes.txt".to_string()]), &["notes.txt".to_string()]));
        assert_eq!(Mode::parse(&json!(true)), Some(Mode::On)); assert_eq!(Mode::parse(&json!("OFF")), Some(Mode::Off)); assert_eq!(Mode::parse(&json!("auto")), Some(Mode::Auto)); assert_eq!(Mode::parse(&json!("maybe")), None);
    }
    #[test]
    fn the_setting_is_kept_in_a_file_and_an_unreadable_one_means_auto() {
        let dir = std::env::temp_dir().join(format!("antiloki-design-mode-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("settings-qa.json");
        assert_eq!(mode_at(&p), Mode::Auto, "no file");
        set_mode_at(&p, Mode::Off).unwrap(); assert_eq!(mode_at(&p), Mode::Off);
        std::fs::write(&p, r#"{"other":1,"design":"on"}"#).unwrap(); set_mode_at(&p, Mode::On).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("\"other\""), "a key the setting does not own survives");
        std::fs::write(&p, "not json").unwrap(); assert_eq!(mode_at(&p), Mode::Auto, "garbage -> auto");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_design_round_runs_once_after_qa_passes_inside_the_cap_and_never_otherwise() {
        assert_eq!(decide(&case()), Decision::Critique);
        let skip = |f: &dyn Fn(&mut Case)| { let mut c = case(); f(&mut c); decide(&c) };
        assert!(matches!(skip(&|c| c.enabled = false), Decision::Skip(w) if w.contains("off")));
        assert!(matches!(skip(&|c| c.measured = false), Decision::Skip(w) if w.contains("measured")));
        assert!(matches!(skip(&|c| c.gate_unmet = 1), Decision::Skip(w) if w.contains("behaviour")), "behaviour first");
        assert!(matches!(skip(&|c| c.rounds_spent = 1), Decision::Skip(w) if w.contains("spent")), "one round per run");
        assert!(matches!(skip(&|c| c.qa_rounds = 2), Decision::Skip(w) if w.contains("cap")), "the QA cap is never exceeded");
        assert!(matches!(skip(&|c| c.max_rounds = 0), Decision::Skip(w) if w.contains("cap")), "send-backs off -> no critique to act on");
        assert!(matches!(skip(&|c| c.seat_available = false), Decision::Skip(w) if w.contains("seat")));
        // one QA round used, one left: still allowed
        assert_eq!(skip(&|c| c.qa_rounds = 1), Decision::Critique);
    }

    #[test]
    fn the_critic_is_the_cheapest_seat_with_eyes_that_can_take_a_call_and_not_necessarily_the_builder() {
        let all = |_: Engine, _: Option<&str>| true;
        // by price: Gemini flash ($0.14/M in) is the cheapest eye there is, when it can take a call
        let pick = critic_seat_with(all, |e, m| critique_price(e, m, 3)).unwrap();
        assert_eq!(pick, (Engine::Gemini, "gemini-3.8-flash-medium"), "{pick:?}");
        // Gemini out of quota (it is, until ~2026-10-04): Composer ($0.50/M in) is cheaper than Grok-high, Sonnet and Codex
        let no_gemini = |e: Engine, _: Option<&str>| e != Engine::Gemini;
        assert_eq!(critic_seat_with(no_gemini, |e, m| critique_price(e, m, 3)), Some((Engine::Cursor, "composer-2.5")));
        // Composer switched off / out of quota too: the next cheapest with vision, never a seat without measured eyes (Opus has none)
        let no_composer = |e: Engine, m: Option<&str>| e != Engine::Gemini && !(e == Engine::Cursor && m == Some("composer-2.5"));
        let pick = critic_seat_with(no_composer, |e, m| critique_price(e, m, 3)).unwrap();
        assert!(crate::looks::VISION_SEATS.contains(&pick) && pick != (Engine::Cursor, "composer-2.5"), "{pick:?}");
        assert_ne!(pick.1, "opus", "Opus has not been measured reading a screenshot");
        // a pin names one of the measured seats and never widens the list
        assert!(pinned(Some("sonnet"), Engine::ClaudeCode, "sonnet") && pinned(Some("claude-code:sonnet"), Engine::ClaudeCode, "sonnet") && !pinned(Some("sonnet"), Engine::Cursor, "composer-2.5") && pinned(None, Engine::Cursor, "composer-2.5") && pinned(Some(" "), Engine::Cursor, "composer-2.5"));
        let only_opus = |e: Engine, m: Option<&str>| pinned(Some("opus"), e, m.unwrap_or(""));
        assert_eq!(critic_seat_with(only_opus, |e, m| critique_price(e, m, 3)), None, "opus is not a measured seat, a pin cannot make it one");
        // nobody can take a call
        assert_eq!(critic_seat_with(|_, _| false, |e, m| critique_price(e, m, 3)), None);
        // a tie in price goes to the gate's own order
        assert_eq!(critic_seat_with(all, |_, _| 1.0), Some((Engine::Cursor, "cursor-grok-4.6-high")));
    }

    #[test]
    fn fixes_are_read_from_the_last_json_object_capped_at_five_and_vague_or_repeated_ones_are_dropped() {
        let text = r#"Looking at the page I see a few things. {"note":"draft"} Final answer:
{"fixes":[
 {"axis":"Spacing","where":"toolbar","problem":"it touches the grid","fix":"increase the gap between the toolbar and the grid to 24px","evidence":"lint spacing_scale: 6px"},
 {"axis":"colour","where":"card subtitle","problem":"2.9:1","fix":"darken the subtitle to #595959 (4.5:1)","evidence":"lint text_contrast"},
 {"axis":"polish","where":"buttons","problem":"three styles","fix":"give Add, Checkout and Clear one shared style"},
 {"axis":"polish","where":"x","problem":"y","fix":"short"},
 {"axis":"polish","where":"buttons","problem":"dup","fix":"give Add, Checkout and Clear one shared style"},
 "set the heading to 32px / 700 and the body to 16px / 400",
 {"axis":"typography","where":"h2","problem":"p","fix":"h2 to 24px, h3 to 18px, body to 16px, nothing in between"},
 {"axis":"typography","where":"label","problem":"p","fix":"label to 14px with 0.02em tracking uppercase"}]}"#;
        let f = parse_fixes(text).unwrap();
        assert_eq!(f.len(), MAX_FIXES, "five at most: {f:#?}");
        assert_eq!(f[0].axis, "spacing", "the axis is lower-cased"); assert_eq!(f[0].r#where, "toolbar");
        assert!(f.iter().all(|x| x.fix.chars().count() >= 8), "a vague one (\"short\") is dropped");
        assert_eq!(f.iter().filter(|x| x.fix.starts_with("give Add")).count(), 1, "a repeated fix is kept once");
        assert!(f[3].fix.starts_with("set the heading"), "a bare string is a fix with no frame: {:?}", f[3]);
        // an empty list is an answer, not a failure
        assert_eq!(parse_fixes(r#"{"fixes": []}"#).unwrap(), vec![]);
        // prose only is unreadable, and says what it got
        let e = parse_fixes("The page looks fine to me.").unwrap_err(); assert!(e.contains("looks fine"), "{e}");
        assert!(parse_fixes(r#"{"winner":"A"}"#).is_err(), "a judge's verdict is not a critique");
    }

    #[test]
    fn a_fix_is_one_numbered_line_with_the_value_in_it() {
        let f = Fix { axis: "colour".into(), r#where: "card subtitle".into(), problem: "2.9:1 on its card".into(), fix: "darken to #595959 (4.5:1)".into(), evidence: "text_contrast .sub".into() };
        let l = f.line(2);
        assert!(l.starts_with("#2 [colour] card subtitle: 2.9:1 on its card -> darken to #595959 (4.5:1)") && l.contains("(seen: text_contrast .sub)") && !l.contains('\n'), "{l}");
        let long = Fix { fix: "x ".repeat(400), ..Default::default() };
        assert!(long.line(1).chars().count() < 400, "bounded");
    }

    #[test]
    fn the_page_as_the_critic_saw_it_is_kept_inside_the_box_only() {
        let d = std::env::temp_dir().join(format!("antiloki-keep-before-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        let (cwd, dest) = (d.join("box"), d.join("kept"));
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        std::fs::write(cwd.join("index.html"), "<p>hi</p>").unwrap(); std::fs::write(cwd.join("src/app.css"), "p{}").unwrap(); std::fs::write(cwd.join("big.bin"), vec![0u8; 1_100_000]).unwrap();
        std::fs::write(d.join("secret.txt"), "outside").unwrap();
        let files: Vec<String> = ["index.html", "src/app.css", "big.bin", "missing.html", "../secret.txt", "/etc/hostname", "./index.html"].iter().map(|s| s.to_string()).collect();
        assert_eq!(keep_before(&cwd.to_string_lossy(), &files, &dest), 2, "the two real small files; nothing outside the box, nothing huge, nothing missing");
        assert_eq!(std::fs::read_to_string(dest.join("src/app.css")).unwrap(), "p{}"); assert!(dest.join("index.html").is_file() && !dest.join("big.bin").exists() && !d.join("secret.txt").to_string_lossy().is_empty());
        assert!(!dest.join("../secret.txt").canonicalize().map(|p| p == dest.canonicalize().unwrap().join("secret.txt")).unwrap_or(false));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_shots_the_critic_sees_are_the_last_three() {
        let s: Vec<String> = (1..=5).map(|i| format!("/e/{i}.png")).collect();
        assert_eq!(pick_shots(&s), vec!["/e/3.png", "/e/4.png", "/e/5.png"]);
        assert_eq!(pick_shots(&s[..2]), vec!["/e/1.png", "/e/2.png"]);
        assert!(pick_shots(&[]).is_empty());
    }

    #[test]
    fn the_critique_prompt_quotes_the_one_rubric_the_shots_and_the_lint_and_asks_for_five_at_most() {
        let p = critique_prompt("a search page", &["/e/1.png".into(), "/e/2.png".into()], "FAIL text_contrast (4.5:1) - e.g. .sub");
        assert!(p.contains(crate::prompts::design_rubric().trim()), "the rubric is the registered one, whole");
        assert!(p.contains("/e/1.png") && p.contains("/e/2.png") && p.contains("a search page") && p.contains("FAIL text_contrast"), "{p}");
        assert!(p.contains("At most 5") || p.contains("at most five") || p.contains("at most 5"), "the cap is in the prompt");
        assert!(p.contains("\"fixes\""), "the answer shape is in the prompt");
        assert!(!p.contains("{{"), "no placeholder left unfilled: {p}");
    }

    #[test]
    fn the_state_remembers_the_first_score_and_reports_the_notes_with_the_before_and_after() {
        let mut d = DesignState::default();
        assert_eq!(d.report_block(), None, "no round, no notes");
        d.score_first = Some(0.55); d.score = Some(0.91); d.rounds = 1; d.seat = "cursor/composer-2.5".into(); d.usd = 0.0123; d.sent = true;
        d.fixes = vec![Fix { axis: "spacing".into(), r#where: "toolbar".into(), problem: "cramped".into(), fix: "gap 24px".into(), evidence: String::new() }];
        let b = d.report_block().unwrap();
        assert!(b.contains("DESIGN NOTES") && b.contains("1 fix(es) from cursor/composer-2.5") && b.contains("+$0.0123") && b.contains("0.55 -> 0.91") && !b.contains("not applied"), "{b}");
        d.sent = false; d.blockers = vec!["text contrast: 1 text node(s) are below 3:1".into()];
        let b = d.report_block().unwrap();
        assert!(b.contains("[not applied]") && b.contains("still failing a design rule"), "{b}");
        assert!(d.unsent_lines().len() == 1); d.sent = true; assert!(d.unsent_lines().is_empty());
    }

    #[test]
    fn the_report_says_when_a_later_send_back_made_the_design_worse() {
        let mut d = DesignState::default();
        d.rounds = 1; d.score_first = Some(0.55); d.score_best = Some(1.0); d.score = Some(0.89); d.sent = true;
        assert!(d.report_block().unwrap().contains("a later send-back made the design worse"), "{:?}", d.report_block());
        d.score = Some(1.0);
        assert!(!d.report_block().unwrap().contains("made the design worse"), "no regression, no note");
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    fn fix(w: &str) -> Fix { Fix { axis: "spacing".into(), r#where: w.into(), problem: "p".into(), fix: "f".into(), evidence: String::new() } }
    #[test]
    fn a_fix_about_an_element_the_change_never_touched_is_noted_not_sent_back() {
        // run #360 "the cart looks ugly": the diff restyled the cart; the critic wanted button.chapter-pill 1 px taller
        let diff = ".cart-panel { padding: 16px }\n.cart-line .qty { gap: 8px }";
        let (inn, out) = partition_in_scope(vec![fix("button.chapter-pill"), fix("div.cart-panel header")], diff, "the cart looks ugly", false);
        assert_eq!(inn.iter().map(|f| f.r#where.as_str()).collect::<Vec<_>>(), vec!["div.cart-panel header"]);
        assert_eq!(out.iter().map(|f| f.r#where.as_str()).collect::<Vec<_>>(), vec!["button.chapter-pill"]);
    }
    #[test]
    fn an_element_the_change_added_is_in_scope_whatever_the_ask_says() {
        // run #361 "add dark mode": the worker's own new .theme-toggle is 31 px - that one IS the change's business
        let (inn, out) = partition_in_scope(vec![fix("button.theme-toggle"), fix(".video-control-btn")], "+.theme-toggle { min-height: 31px }", "add dark mode", false);
        assert_eq!(inn.len(), 1); assert_eq!(inn[0].r#where, "button.theme-toggle"); assert_eq!(out.len(), 1);
    }
    #[test]
    fn a_word_of_the_ask_in_the_where_keeps_it_and_an_unidentifiable_where_is_never_dropped() {
        let (inn, out) = partition_in_scope(vec![fix("the checkout bar total"), fix("page"), fix("div")], "", "polish the checkout bar", false);
        assert_eq!(inn.len(), 3, "checkout is the ask's word; `page` and `div` name nothing, so they stay");
        assert!(out.is_empty());
        assert_eq!(partition_in_scope(vec![], "x", "y", false), (vec![], vec![]));
        let (inn, out) = partition_in_scope(vec![fix("button.chapter-pill")], "", "create the shop page", true);
        assert_eq!((inn.len(), out.len()), (1, 0), "a page the run created has no pre-existing parts");
    }
    #[test]
    fn the_report_says_what_was_noticed_and_left_alone() {
        let mut st = DesignState::default(); st.rounds = 1; st.sent = true; st.out_of_scope = vec![fix("button.chapter-pill")];
        let b = st.report_block().unwrap();
        assert!(b.contains("outside this ask - NOT changed") && b.contains("button.chapter-pill"), "{b}");
    }
}

