//! THE DETERMINISTIC SCREEN — abstention on grounds nobody can argue with (operator 2026-09-25: the screening gate
//! "needs to be more often used, it is like the model is always sure when i use it").
//!
//! It is always sure because we told it to be. `goal.router_plan` says a blocking question is "the exception, not the
//! rule: most goals plan without a question" — and the model obeys. Tuning that sentence would trade one unmeasured
//! bias for another; the model would be doubtful because the prompt asked for doubt, not because the ask was unclear.
//!
//! So the doubts that MUST stop a run are found here instead, in code, from facts about the repository:
//!   · a path the ask names that does not exist and the ask does not say to create — the single most common way a
//!     plan goes confidently wrong, and a `Path::exists` answers it;
//!   · an ask too thin to plan: a handful of words, no file, no acceptance — "fix the login" is not a specification;
//!   · a reference with no antecedent: "do it the same way", "like that one", when nothing in the ask says which.
//!
//! Each doubt carries its EVIDENCE, so the question the operator sees names the path or the phrase rather than asking
//! them to guess what the machine found confusing. Costs nothing: no model call, no network, pure string and filesystem
//! work — which is also why it can run before the planner instead of hoping the planner volunteers.
//!
//! WHAT IT WILL NOT DO. It does not doubt on style, on length alone, or on anything it cannot point at. A screen that
//! fires on everything is switched off within a day, and then the gate is worse than it was.
use serde_json::{json, Value};

/// one thing the ask does not settle, and the evidence for saying so
#[derive(Clone, Debug, PartialEq)]
pub struct Doubt {
    pub kind: &'static str,
    /// the question to put to the operator, naming what was found
    pub text: String,
    /// the exact token or phrase the doubt is about — never a paraphrase
    pub evidence: String,
    /// what the run would assume if the operator says nothing
    pub default: String,
    /// THE ANSWERS AS CHIPS, NOT AN EMPTY BOX (operator 2026-09-28: "is chips selection response with a last chip
    /// of write yourself. Not an input field just"). A blank field asks the operator to compose a specification
    /// for a machine; a chip asks them to make a choice, which is the thing they actually have in their head. The
    /// screen is a conversation, so the FIRST chip is always the default — the answer that needs no thought.
    /// The escape from a closed set is the FRONT END's ("✎ write it yourself", `heimdall-rail.tsx`), which it
    /// appends to every question it draws — including a planner-authored one that listed no options at all. This
    /// module must not claim to do it: the doc said so for a day and it was not true here (audit 2026-09-28).
    pub options: Vec<String>,
}

/// FILE-SHAPED TOKENS IN AN ASK. The same rule `cli::classify_ask` counts with, kept in one place: a token with a path
/// separator, or `stem.ext` with a short alphanumeric extension. A sentence's full stop is not an extension and a URL
/// is not a path. Pure.
pub fn paths_in(text: &str) -> Vec<String> {
    // ENGLISH ABBREVIATIONS ARE NOT FILES. `e.g` survives every structural rule — a one-letter stem and a
    // one-letter extension — and it is one of the commonest tokens in a written ask.
    const NOT_FILES: &[&str] = &["e.g", "i.e", "etc", "vs", "cf", "al", "viz", "resp", "approx"];
    let is_file = |w: &str| -> bool {
        if w.starts_with("http") || w.starts_with("www.") { return false }
        if NOT_FILES.contains(&w.to_ascii_lowercase().as_str()) { return false }
        // a path is made of something, not only punctuation: `//` is a comment marker, not a directory
        if !w.chars().any(|c| c.is_ascii_alphanumeric()) { return false }
        if w.contains('/') {
            // A FRACTION AND A DATE ARE NOT PATHS: `1/5`, `10/2`, `06/04/2020`. No source path is all digits.
            if w.split('/').all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit())) { return false }
            // KNOWN LIMIT, measured not guessed (HumanEvalComm, 2026-09-26): `input/output` and `True/False` still
            // read as paths, as do dotted identifiers like `math.pow`. They are genuinely ambiguous tokens — a real
            // ask naming `src/main` looks identical — and after the numeric and decimal rules they account for 4 of
            // 164 clear problems. Narrowing further would start rejecting real paths, which is the error that
            // matters more: this gate exists to catch a named file that is not there.
            return !w.ends_with('/') || w.len() > 1
        }
        match w.rsplit_once('.') {
            // A DECIMAL NUMBER IS NOT A FILE (found 2026-09-26 by running HumanEvalComm against this gate: 85 of the
            // 94 tokens it flagged in clear problem statements were numbers like `2.0`, `0.5`, `3.14`). `0.5` fits
            // the shape exactly — stem `0`, extension `5`, both alphanumeric — so an ask saying "set the threshold
            // to 0.5" raised a BLOCKING doubt about a file named `0.5` and stopped the run to ask about it. Every
            // real extension contains a letter; no decimal does. That one condition removes the whole class and
            // cannot reject a real file.
            Some((stem, ext)) => !stem.is_empty() && (1..=4).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic())
                && stem.chars().last().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false),
            None => false,
        }
    };
    let mut out: Vec<String> = text
        .split(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '\'' | ',' | '(' | ')' | '[' | ']' | '<' | '>' | ';'))
        .map(|w| w.trim_matches(|c: char| matches!(c, '.' | ':' | '!' | '?')).to_string())
        .filter(|w| !w.is_empty() && is_file(w))
        .collect();
    out.sort(); out.dedup();
    out
}

/// does the ask itself say to create the thing? Then a missing path is the POINT, not a doubt.
///
/// MATCHED ON WORD BOUNDARIES, because a substring match here silently switches the screen off: `"port "` is inside
/// "su**pport** the new flag" and "trans**port** layer", so an ask containing either word stopped doubting a path that
/// was not there — the exact failure this module exists to prevent, hidden inside the module itself (found auditing
/// 2026-09-26). Pure.
pub fn asks_to_create(text: &str) -> bool {
    // the verbs and phrases are per-language data (lexicon.rs), matched as whole words / phrases of the normalised text
    let n = crate::lexicon::norm(text);
    crate::lexicon::all().creates.iter().any(|k| crate::lexicon::has_phrase(&n, k))
}

/// does the ask MOVE or RENAME something? Then the path it names last is the destination, which cannot exist yet - only the first (the source) is worth doubting.
pub fn asks_to_relocate(text: &str) -> bool {
    let n = crate::lexicon::norm(text);
    crate::lexicon::all().relocates.iter().any(|k| crate::lexicon::has_phrase(&n, k))
}

/// THE PHRASES THAT POINT AT NOTHING. Only counted when the ask gives them nothing to point AT: an ask that names a
/// file and then says "the same way" is usually clear, so the check requires both a dangling phrase and an ask with no
/// concrete referent at all. Pure.
fn dangling(text: &str, paths: &[String]) -> Option<String> {
    if !paths.is_empty() { return None }
    let n = crate::lexicon::norm(text);
    crate::lexicon::all().dangling.iter().find(|k| crate::lexicon::has_phrase(&n, &crate::lexicon::norm(k))).cloned()
}

/// A QUESTION WHOSE ANSWER CANNOT CHANGE THE WORK (operator 2026-09-28). "Anything particular in mind, or shall antiloki
/// decide?", "do you have a preference?", options that are all "you decide" or that say the same thing twice: asking
/// them stops a run to learn nothing. Such a question is taken by its default and stated as an assumption instead.
/// Pure — tested.
pub fn is_generic(text: &str, options: &[String]) -> bool {
    let t = text.to_ascii_lowercase();
    let norm = |o: &String| o.to_ascii_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    let mut seen: Vec<String> = vec![];
    for o in options { let n = norm(o); if !seen.contains(&n) { seen.push(n); } }
    let all_yours = |seen: &[String]| seen.iter().all(|o| o.contains("antiloki decides") || o.contains("keep it minimal") || o.contains("smallest thing") || o.contains("you decide") || o.contains("your call"));
    // the WORDS "anything particular in mind" only condemn a question that offers nothing else: "which sort order did you have in
    // mind — price or name?" has two different outcomes and is a real question (audit 2026-09-28)
    let says_it = ["anything particular", "in mind", "shall antiloki decide", "should antiloki decide", "any preference", "any particular", "anything specific", "any specific"].iter().any(|k| t.contains(k));
    if says_it { return seen.len() < 2 || all_yours(&seen); }
    !seen.is_empty() && all_yours(&seen)
}

/// THE SCREEN. `exists` answers "is this path in the box?" — injected so the whole function is pure and testable
/// without a filesystem. Returns the doubts in the order they should be asked, at most three: a question round is a
/// conversation, not an interrogation, and the plan's own gate caps at three.
pub fn doubts<F: Fn(&str) -> bool>(goal: &str, exists: F) -> Vec<Doubt> {
    let g = goal.trim();
    let mut out: Vec<Doubt> = vec![];
    let paths = paths_in(g);

    // 1 · A NAMED PATH THAT IS NOT THERE. The ask is specific, and specifically wrong — which is worse than vague,
    // because a planner will happily invent a neighbouring file and be confidently wrong about the whole run.
    if !asks_to_create(g) {
        // A SLASH IS NOT ALWAYS A PATH (found live 2026-09-29: the front wrote "…display branding/navigation…" and the gauge asked whether to
        // create a folder called `branding/navigation`). A token with a file extension is a file whether or not its folder exists; one WITHOUT
        // an extension is a path only when its first folder is really there - otherwise it is a phrase ("and/or", "branding/navigation").
        let is_path = |p: &str| p.rsplit('/').next().map(|f| f.contains('.')).unwrap_or(false) || exists(p.split('/').next().unwrap_or(p));
        // `paths_in` does not keep the order of the ask, so a move/rename's SOURCE is the path that comes first in the TEXT
        let mut named: Vec<&String> = paths.iter().filter(|p| is_path(p)).collect();
        named.sort_by_key(|p| g.find(p.as_str()).unwrap_or(usize::MAX));
        let cap = if asks_to_relocate(g) { 1 } else { 3 };
        for p in named.into_iter().take(cap) {
            if !exists(p) {
                out.push(Doubt {
                    kind: "missing-path",
                    text: format!("`{p}` is not in this box — should it be created, or did you mean another one?"),
                    evidence: p.clone(),
                    default: format!("create `{p}`"),
                    // FIVE ANSWERS THAT ARE ABOUT A MISSING FILE (a generic "where should it go" chip made no sense here)
                    options: vec![format!("create `{p}`"), "find the closest existing file and use that".into(), "look for it under another folder first".into(),
                                  "skip that file and do the rest of the ask".into(), "change nothing until I confirm the path".into()],
                });
            }
        }
    }

    // 2 · (REMOVED 2026-09-28) AN ASK TOO THIN TO PLAN used to ask "anything particular in mind, or shall antiloki decide
    // the details?" with two answers that both meant "you decide". Operator: "screening is to make vague prompts
    // sharper, not to ask useless questions … if this is the question, screening wasn't needed at this task". A question
    // whose every answer is "your call" sharpens nothing; the planner reads the code and states what it assumed, which is
    // the same outcome without stopping the run. A doubt stays only when the answer can change the work.

    // 3 · A REFERENCE TO SOMETHING THE ASK NEVER NAMED.
    if let Some(phrase) = dangling(g, &paths) {
        out.push(Doubt {
            kind: "dangling",
            text: format!("“{phrase}” — which one? The ask does not name it and neither does this box."),
            evidence: phrase,
            default: "the most recent comparable change in this box".into(),
            options: vec!["the most recent comparable change in this box".into(),
                          "the one the last episode was about".into(),
                          "the closest existing example in this codebase".into(),
                          "none of them — I will describe it".into()],
        });
    }

    out.truncate(3);
    // EVERY QUESTION CARRIES 3–7 CHIPS THAT DO NOT CONTRADICT EACH OTHER OR THE ASK (operator 2026-09-29) — the same rule
    // the dedicated screen is held to; both doubts here are questions of where/which
    for d in out.iter_mut() { d.options = crate::screen_kinds::chips_for(None, &d.options, g); if let Some(f) = d.options.first() { d.default = f.clone(); } }
    out
}

/// the doubts as the screening gate's own question shape (`goals::Question`), so the pane that already renders the
/// model's questions renders these with no change: same ids, same certainty, same answer round.
pub fn as_questions(doubts: &[Doubt]) -> Vec<Value> {
    doubts.iter().enumerate().map(|(i, d)| json!({
        "id": format!("d{}", i + 1),
        "text": d.text,
        "options": d.options,
        "default": d.default,
        "certainty": "blocking",
        "found_by": "screen",
        "evidence": d.evidence,
        "doubt": d.kind,
        "kind": "target",
    })).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn none(_: &str) -> bool { false }
    fn all(_: &str) -> bool { true }

    #[test]
    fn the_destination_of_a_move_or_rename_is_not_a_missing_path() {
        let ex = |p: &str| ["src", "src/App.tsx", "src/products.ts"].contains(&p);
        for t in ["rename src/products.ts to src/catalog.ts", "renomeie src/products.ts para src/catalogo.ts", "move src/App.tsx to src/app/App.tsx", "mueve src/products.ts a src/data/products.ts"] { assert!(doubts(t, ex).is_empty(), "{t}"); }
        // …but a missing SOURCE is still a doubt, whichever way the words are ordered
        assert_eq!(doubts("rename src/nope.ts to src/catalog.ts", ex).iter().map(|d| d.evidence.as_str()).collect::<Vec<_>>(), vec!["src/nope.ts"]);
        // and the ask's own language decides "create": the destination of a create is the point, in every pack
        assert!(doubts("crie src/hooks/useCart.ts com um hook", ex).is_empty() && doubts("erstelle src/neu.ts mit einer Funktion", ex).is_empty());
    }
    #[test]
    fn a_slash_phrase_is_not_a_missing_path() {
        // found live: "display branding/navigation" is prose, not a folder to create
        assert!(doubts("update the Header to display branding/navigation links", none).is_empty());
        assert!(doubts("support read/write access and/or admin rights", none).is_empty());
        // a file is a file whether or not its folder exists; a real folder's child is checked
        assert_eq!(doubts("fix the bug in nofolder/thing.ts", none).len(), 1);
        let src_only = |p: &str| p == "src";
        assert_eq!(doubts("split the code in src/utils", src_only).len(), 1);
    }

    #[test]
    fn a_named_path_that_is_not_there_stops_the_run_and_names_it() {
        let d = doubts("in serve/src/nope.rs fix the off-by-one", none);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, "missing-path");
        assert_eq!(d[0].evidence, "serve/src/nope.rs");
        assert!(d[0].text.contains("serve/src/nope.rs"), "the question names the path: {}", d[0].text);
        // …and when the file IS there, there is nothing to ask
        assert!(doubts("in serve/src/nope.rs fix the off-by-one", all).is_empty());
    }

    #[test]
    fn an_ask_that_says_to_create_the_file_is_not_doubted_for_its_absence() {
        // the missing path is the POINT of the ask — doubting it would fire on every new file anyone ever writes
        assert!(doubts("create serve/src/brand-new.rs with the parser", none).is_empty());
        assert!(doubts("add a new file src/x.ts exporting nothing yet", none).is_empty());
    }

    /// FOUND BY AUDITING (2026-09-26): `"port "` is a substring of "su**pport**" and "trans**port**", so an ask
    /// containing either word turned the missing-path doubt off — the screen disabling itself on a word that has
    /// nothing to do with creating a file.
    #[test]
    fn a_word_that_merely_contains_a_verb_does_not_switch_the_screen_off() {
        let d = doubts("support the new flag in serve/src/nope.rs", none);
        assert_eq!(d.len(), 1, "\"support\" is not \"port\"");
        assert_eq!(d[0].kind, "missing-path");
        assert_eq!(doubts("fix the transport layer in serve/src/nope.rs", none).len(), 1, "nor is \"transport\"");
        // …while the verbs themselves still mean what they meant
        assert!(doubts("port the parser to serve/src/new.rs", none).is_empty());
        assert!(doubts("extract the helper into serve/src/new.rs", none).is_empty());
        assert!(doubts("generate serve/src/new.rs from the schema", none).is_empty());
    }

    #[test]
    fn a_thin_ask_is_not_doubted_the_planner_assumes_and_says_so() {
        // "fix the login" / "add a button" used to stop for "anything particular in mind?" — a question whose every answer was "you decide"
        assert!(doubts("fix the login", none).is_empty());
        assert!(doubts("add a button", none).is_empty());
        assert!(doubts("make the login redirect so that expired sessions return 401", none).is_empty());
        assert!(doubts("tidy src/a.ts", all).is_empty());
        assert!(doubts("   ", none).is_empty());
    }

    #[test]
    fn a_question_that_cannot_change_the_work_is_generic() {
        let o = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(is_generic("“add a button” — anything particular in mind, or shall antiloki decide the details?", &o(&[])));
        assert!(is_generic("Which way?", &o(&["antiloki decides the details, in keeping with what is already there", "keep it minimal — the smallest thing that satisfies the ask", "antiloki decides the details, in keeping with what is already there"])));
        // a real question: the options are different outcomes
        assert!(!is_generic("Which sort order did you have in mind?", &o(&["price low to high", "name a to z"])), "the phrase alone does not condemn a question with two real outcomes");
        assert!(!is_generic("Should the filter reset when the user leaves the page?", &o(&["reset on leave", "remember the last filter"])));
        assert!(!is_generic("`src/nope.rs` is not in this box — should it be created?", &o(&["create it", "use the closest file"])));
    }

    #[test]
    fn a_dangling_reference_is_doubted_only_when_nothing_in_the_ask_anchors_it() {
        let d = doubts("do it the same way and keep the tests green", none);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, "dangling");
        // the same phrase with a file named is ordinary English, not an ambiguity
        assert!(doubts("in src/a.ts do it the same way as the sibling", all).is_empty());
    }

    /// operator 2026-09-28: "screening NEVER asks about the code, asks about the idea if is unsure … is chips
    /// selection response with a last chip of write yourself. Not an input field just"
    #[test]
    fn a_doubt_asks_about_the_idea_and_offers_chips_never_a_bare_box() {
        let found = doubts("do it the same way and keep the tests green", |_| false);
        assert_eq!(found.len(), 1, "a reference to nothing is a doubt whose answer changes the work");
        let d = &found[0];
        for banned in ["which files", "file", "how will we know", "worked?", "test", "acceptance"] {
            assert!(!d.text.to_ascii_lowercase().contains(banned),
                    "the screen asked about the CODE: {:?} contains {banned:?}", d.text);
        }
        assert!(d.text.contains("the same way"), "the phrase is quoted back or the question floats free");
        assert!(d.options.len() >= 2, "an answer is a choice, not a composition: {:?}", d.options);
        assert_eq!(d.options[0], d.default, "the first chip is the answer that needs no thought");

        // every doubt the screen can raise carries chips — a bare box is never the only way to answer
        for d in doubts("fix src/nope.ts", |_| false) { assert!(!d.options.is_empty(), "{} has no chips", d.kind); }
        for d in doubts("do it the same way", |_| false) { assert!(!d.options.is_empty(), "{} has no chips", d.kind); }

        // …and they survive the conversion the run actually uses
        let q = as_questions(&found);
        assert_eq!(q[0]["options"].as_array().unwrap().len(), d.options.len(),
                   "the chips were dropped on the way to the operator");
    }

    #[test]
    fn it_refuses_to_interrogate() {
        let many = doubts("fix a.ts and b.ts and c.ts and d.ts and e.ts", none);
        assert!(many.len() <= 3, "at most three questions, however many things are missing: {}", many.len());
    }

    #[test]
    fn paths_in_reads_paths_and_not_prose() {
        assert_eq!(paths_in("in `serve/src/x.rs`, fix it."), vec!["serve/src/x.rs"]);
        assert_eq!(paths_in("rename a.ts to b.ts"), vec!["a.ts", "b.ts"]);
        assert!(paths_in("this sentence ends. and continues").is_empty(), "a full stop is not an extension");
        assert!(paths_in("see https://example.com/x.md for context").is_empty(), "a URL is not a path in this box");

        // A DECIMAL IS NOT A FILE — the false alarm HumanEvalComm exposed (2026-09-26). `0.5` fits the shape
        // exactly, so this gate used to BLOCK a run to ask about a file named `0.5`.
        assert!(paths_in("set the threshold to 0.5").is_empty(), "a decimal is not a path");
        for n in ["2.0", "0.25", "3.14", "-0.5", "15.0", "1.7"] {
            assert!(paths_in(&format!("use {n} here")).is_empty(), "{n} was read as a file");
        }
        // …and the fix does not cost a real file: every extension has a letter in it
        assert_eq!(paths_in("open data.7z and main.rs"), vec!["data.7z", "main.rs"]);
        assert_eq!(paths_in("a.c compiles"), vec!["a.c"], "a one-letter extension is still an extension");

        // English abbreviations and punctuation are not paths either
        assert!(paths_in("rename it, e.g. to something clearer").is_empty(), "e.g is not a file");
        assert!(paths_in("i.e. the second one").is_empty());
        assert!(paths_in("the // comment marker").is_empty(), "// is not a directory");
        assert!(paths_in("a 1/5 chance on 06/04/2020").is_empty(), "a fraction and a date are not paths");
        assert_eq!(paths_in("look in src/main"), vec!["src/main"], "…and a real extensionless path still counts");
    }

    /// THE DETERMINISTIC GATE, MEASURED AGAINST A PUBLISHED BENCHMARK (2026-09-26).
    ///
    /// HumanEvalComm is 164 clear problems and their underspecified variants — ambiguous (`a`), inconsistent (`c`),
    /// incomplete (`p`). That is antiloki's `screening` job stated by somebody else, so it is the honest way to ask
    /// what THIS gate actually covers.
    ///
    /// It is a MEASUREMENT, not an assertion, and it is `#[ignore]`d for two reasons: it needs a dataset that is not
    /// in this repository, and a gate that scores badly here is not a broken gate — `doubts()` was built for asks
    /// about a repository (a path that is not there, a reference to nothing, an ask too thin to act on), and a
    /// HumanEval function spec mentions no files at all. What the number tells us is the SPLIT: how much of
    /// screening the deterministic half can carry, and how much only a model can. Printing that honestly is worth
    /// more than a green tick.
    ///
    ///   cargo test -p antiloki-serve --lib humanevalcomm -- --ignored --nocapture
    #[test]
    #[ignore = "needs ~/.antiloki/bench/human-eval-comm; prints a measurement, asserts nothing about quality"]
    fn the_deterministic_gate_on_humanevalcomm() {
        let path = format!("{}/.antiloki/bench/human-eval-comm/Benchmark/HumanEvalComm.jsonl",
                           std::env::var("HOME").unwrap_or_default());
        let Ok(text) = std::fs::read_to_string(&path) else {
            println!("no dataset at {path} — clone jie-jw-wu/human-eval-comm there"); return;
        };
        // no file exists in these prompts' world: they are standalone function specs
        let none = |_: &str| false;
        let (mut clear, mut clear_asked, mut modified, mut modified_asked) = (0, 0, 0, 0);
        let mut clear_kinds: std::collections::BTreeMap<&str, i32> = Default::default();
        let mut mod_kinds: std::collections::BTreeMap<&str, i32> = Default::default();
        for line in text.lines().filter(|l| l.starts_with('{')) {
            let Ok(row) = serde_json::from_str::<Value>(line) else { continue };
            let Some(obj) = row.as_object() else { continue };
            for (key, val) in obj {
                // `prompt` is the original, clear problem; `prompt1a`/`prompt2ac`/… are its underspecified variants
                if !key.starts_with("prompt") { continue }
                let Some(text) = val.as_str() else { continue };
                let ds = doubts(text, none);
                let asked = !ds.is_empty();
                let bucket = if key == "prompt" { &mut clear_kinds } else { &mut mod_kinds };
                for d in &ds { *bucket.entry(d.kind).or_default() += 1; }
                if key == "prompt" { clear += 1; clear_asked += i32::from(asked); }
                else { modified += 1; modified_asked += i32::from(asked); }
            }
        }
        // THE 2x2. The second row is the one a product lives or dies by: a gate that asks about a problem that was
        // never ambiguous turns every run into an interview.
        println!("\nHumanEvalComm · the DETERMINISTIC gate only (screen::doubts)");
        println!("  underspecified: asked {modified_asked}/{modified}  ({:.0}% — caught)",
                 100.0 * modified_asked as f64 / modified.max(1) as f64);
        println!("  clear:          asked {clear_asked}/{clear}  ({:.0}% — false alarms, lower is better)",
                 100.0 * clear_asked as f64 / clear.max(1) as f64);
        println!("  what this measures: how much of screening needs no model at all. The rest is the model's half.");
        println!("  by kind — clear: {clear_kinds:?}");
        println!("  by kind — underspecified: {mod_kinds:?}");
    }

    /// THE MODEL HALF OF SCREENING, ON THE SAME BENCHMARK — the other side of the split the test above measures.
    ///
    /// It reuses the two things that actually ship: `goals::plan_prompt_screened` renders the REAL planner+screen
    /// prompt, and `goals::is_question_reply` is the REAL decision. Writing a prompt for the benchmark would have
    /// measured something antiloki does not do.
    ///
    /// THE 2x2, and the second row is the one a product lives or dies by:
    ///   · an underspecified variant SHOULD get a question;
    ///   · the original, clear problem should NOT — a gate that asks about everything turns every run into an
    ///     interview, and that failure is barely scored by the published metric.
    ///
    /// It SPENDS MONEY, so it is `#[ignore]`d and takes its size from the environment:
    ///   SCREEN_BENCH_N=20 SCREEN_BENCH_ENGINE=opencode SCREEN_BENCH_MODEL=opencode/qwen3.6-plus \
    ///     cargo test -p antiloki-serve --lib the_model_half_of_screening -- --ignored --nocapture
    ///
    /// One honest difference from production, stated rather than buried: this calls the CLI directly instead of
    /// going through the engine layer, which adds retries and transport around the same call — not screening
    /// behaviour. And these are standalone function specs with no repository, while the prompt says to read the
    /// code first; an empty box is a fair reading of that, but it is our adaptation, so the row files under suite
    /// `screening` and never as HumanEvalComm's own published number.
    #[test]
    #[ignore = "spends money on a real engine; opt in with SCREEN_BENCH_N"]
    fn the_model_half_of_screening() {
        let n: usize = std::env::var("SCREEN_BENCH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        if n == 0 { println!("set SCREEN_BENCH_N to run this (it calls a real engine and costs money)"); return }
        let engine = std::env::var("SCREEN_BENCH_ENGINE").unwrap_or_else(|_| "opencode".into());
        let model = std::env::var("SCREEN_BENCH_MODEL").unwrap_or_else(|_| "opencode/qwen3.6-plus".into());
        let path = format!("{}/.antiloki/bench/human-eval-comm/Benchmark/HumanEvalComm.jsonl",
                           std::env::var("HOME").unwrap_or_default());
        let Ok(text) = std::fs::read_to_string(&path) else { println!("no dataset at {path}"); return };

        // one CLEAR and one AMBIGUOUS prompt from each of the first `n` problems — paired, so the two rows of the
        // 2x2 are drawn from the same problems and a difference cannot be an artefact of which ones were picked
        let mut pairs: Vec<(String, String)> = vec![];
        for line in text.lines().filter(|l| l.starts_with('{')).take(n) {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let (Some(clear), Some(amb)) = (v["prompt"].as_str(), v["prompt1a"].as_str()) else { continue };
            pairs.push((clear.to_string(), amb.to_string()));
        }
        // AN EMPTY BOX, AND IT HAS TO BE MADE (found 2026-09-26, by distrusting a clean `0/6` on both rows). The
        // test inherits cargo's working directory, so the first run of this harness pointed the engine at THE
        // ANTILOKI REPOSITORY: it globbed `**/*.py`, read `fixtures/fx-python/…`, and planned to put the answer in
        // `serve/candidate.py`. The prompt says to read the code first, and it obediently read code that had
        // nothing to do with the question. The measurement was of the wrong thing entirely.
        let box_dir = std::env::temp_dir().join(format!("antiloki-screen-bench-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&box_dir);

        // what the reply did with an ambiguity — three outcomes, not two
        #[derive(PartialEq, Debug)]
        enum Said { Asked, Assumed, Silent }
        let ask = |goal: &str| -> Option<Said> {
            let prompt = crate::goals::plan_prompt_screened(goal, 4, true, &[], &[], true, false);
            let mut c = crate::proc::cmd(&engine);
            c.current_dir(&box_dir);
            if engine == "opencode" { c.args(["run", "--model", &model, &prompt]); }
            else { c.args(["-p", "--model", &model, "--max-turns", "1", &prompt]); }
            let out = c.output().ok()?;
            let body = String::from_utf8_lossy(&out.stdout).to_string();
            // A ZERO THAT LOOKS LIKE A RESULT IS THE THING TO DISTRUST (2026-09-26). `0/n` on BOTH rows is what a
            // broken harness prints, not only what a silent model does — so the raw reply is one env var away.
            if std::env::var("SCREEN_BENCH_DEBUG").is_ok() {
                println!("--- raw reply ({} bytes, stderr {} bytes) ---\n{}\n---",
                         body.len(), out.stderr.len(), body.chars().take(1200).collect::<String>());
                if !out.stderr.is_empty() { println!("stderr: {}", String::from_utf8_lossy(&out.stderr).chars().take(400).collect::<String>()); }
            }
            if body.trim().is_empty() { return None }
            // THE THIRD OUTCOME, which a two-way count cannot see. The prompt deliberately offers two honest
            // routes: ask when a wrong guess would make the plan materially wrong, or take the obvious reading and
            // SAY SO in `assumptions`. A reply that names its assumption has not failed the way a reply that
            // silently guessed has, and collapsing them scores the prompt working as designed as a miss.
            if crate::goals::is_question_reply(&body) { return Some(Said::Asked) }
            let (_, assumptions) = crate::goals::parse_screen(&body);
            Some(if assumptions.is_empty() { Said::Silent } else { Said::Assumed })
        };
        let (mut c_ask, mut c_asm, mut c_sil) = (0, 0, 0);
        let (mut a_ask, mut a_asm, mut a_sil) = (0, 0, 0);
        for (i, (clear, amb)) in pairs.iter().enumerate() {
            match ask(clear) { Some(Said::Asked) => c_ask += 1, Some(Said::Assumed) => c_asm += 1, Some(Said::Silent) => c_sil += 1, None => {} }
            match ask(amb)   { Some(Said::Asked) => a_ask += 1, Some(Said::Assumed) => a_asm += 1, Some(Said::Silent) => a_sil += 1, None => {} }
            println!("  {}/{} …", i + 1, pairs.len());
        }
        let _ = std::fs::remove_dir_all(&box_dir);
        println!("\nHumanEvalComm · the MODEL half · {engine}:{model} · in an empty box");
        println!("                  asked   assumed+said   silently guessed");
        println!("  underspecified: {a_ask:5}   {a_asm:12}   {a_sil:16}   <- asked or assumed is honest; silent is the miss");
        println!("  clear:          {c_ask:5}   {c_asm:12}   {c_sil:16}   <- asking here is the expensive false alarm");
        println!("  (the deterministic gate alone scores ~2% and ~2% — see the test above)");
    }

    #[test]
    fn the_questions_are_the_gates_own_shape() {
        let q = as_questions(&doubts("in serve/src/nope.rs fix it", none));
        assert_eq!(q[0]["id"], "d1");
        assert_eq!(q[0]["certainty"], "blocking", "a deterministic doubt blocks — it is a fact, not a hunch");
        assert_eq!(q[0]["found_by"], "screen");
        assert!(q[0]["default"].as_str().unwrap().len() > 0, "every question carries what happens if you say nothing");
    }
}
