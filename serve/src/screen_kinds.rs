//! THE KINDS OF QUESTION SCREENING MAY ASK (operator 2026-09-29: "instead of every single time deciding from scratch which
//! category should be asked, we could have a list, a template screening … so the AI won't just go rogue"; "chips always
//! between 3 and 7 … avoid contradictions on the chips").
//!
//! The 27 questions screening has asked on this machine fall into a handful of shapes — an unspecified behaviour, a
//! place or a target, a missing value, nothing to build on, how-will-we-know, a self-contradicting ask, a destructive
//! change. So the screening call is given the LIST: it names the kind, writes the question in that kind's frame, and offers
//! the answers as chips. This module is the other half — the CODE that holds the model to it, so a rogue question cannot
//! reach the operator however the model words it:
//!
//!   · a question with no kind is classified from its words; one that fits none keeps its text but gets no chips it cannot
//!     stand behind (a bare box, plus the front end's "write it yourself"), and is logged;
//!   · the chips are cleaned: trimmed, no duplicate or overlapping pair, no "A or B" (that is two answers), no question
//!     posing as an answer, no "you decide", none that names a different language/framework than the ask already fixed;
//!   · then held to 3–7. Fewer than 3 are filled from the kind's own frame chips; more than 7 are cut. The first chip is the
//!     answer the model would take — it is the default;
//!   · at most one question per kind, at most three questions.
//!
//! The front end appends "write it yourself" to every question; that escape is not counted in the 3–7.
use serde_json::{json, Value};

pub struct Kind {
    pub key: &'static str,
    /// the frame the question is written in — shown to the model
    pub frame: &'static str,
    /// chips that are valid answers to this kind whatever the subject — used only to reach the minimum of 3
    pub pad: [&'static str; 4],
}
pub const MIN_CHIPS: usize = 3;
pub const MAX_CHIPS: usize = 7;
pub const KINDS: [Kind; 7] = [
    Kind { key: "behaviour", frame: "What should <the thing> do or return when <the case the ask leaves open>?",
           pad: ["Follow how the closest existing code behaves", "The simplest behaviour that satisfies the ask, stated in the report", "Strict: fail loudly on anything unexpected", "Lenient: handle anything unexpected quietly"] },
    Kind { key: "target", frame: "Where should <the thing> go, or which <thing> is meant?",
           pad: ["Next to the existing element it relates to most", "In the file that already holds the closest feature", "In a new dedicated file or section", "Wherever the current layout leaves room, stated in the report"] },
    Kind { key: "values", frame: "What <value> should <the thing> use?",
           pad: ["A realistic placeholder that is easy to edit later", "The values of the closest existing example", "Read from an existing config or constant", "Left unset until I provide them"] },
    Kind { key: "foundation", frame: "There is nothing here to build on: which <stack or structure>?",
           pad: ["TypeScript with React (Vite)", "Node.js with Express", "Python with Flask", "Plain HTML, CSS and JavaScript"] },
    Kind { key: "done", frame: "How will we know <the thing> works?",
           pad: ["A test that fails before the change and passes after", "The build and typecheck pass", "The existing test suite still passes", "I check it by hand in the running app"] },
    Kind { key: "conflict", frame: "The ask says <A> but <B> — which one?",
           pad: ["Follow what the ask says", "Follow what the code already does", "Do both wherever they do not collide", "Change nothing until this is settled"] },
    Kind { key: "risk", frame: "This would <delete, overwrite or migrate> <the thing> — replace it or keep it?",
           pad: ["Replace it in place", "Keep the old one and add the new beside it", "Keep the old one behind a flag", "Migrate the data, then remove the old one"] },
];
pub fn kind(key: &str) -> Option<&'static Kind> { KINDS.iter().find(|k| k.key == key.trim().to_ascii_lowercase()) }
/// the list as the screening prompt shows it
pub fn menu() -> String { KINDS.iter().map(|k| format!("- {}: {}", k.key, k.frame)).collect::<Vec<_>>().join("\n") }

/// A KIND FROM THE QUESTION'S OWN WORDS, for a question that named none (or named one that is not on the list). The order is
/// the order of the mistakes: a destructive change and a self-contradiction are worth catching before the general kinds. None when
/// nothing fits — the caller keeps the text and offers no invented chips. Pure.
pub fn classify(text: &str) -> Option<&'static str> {
    let t = text.to_ascii_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| t.contains(w));
    if has(&["delete", "overwrite", "migrat", "drop the", "remove the existing", "replace the existing", "destructive"]) { return Some("risk"); }
    if has(&["contradict", "conflicts with", "but the code", "but the ask", "which of the two", "both say", "inconsistent"]) { return Some("conflict"); }
    if has(&["nothing to build on", "is empty", "no files", "which stack", "which framework", "which language", "scaffold", "from scratch", "no existing app"]) { return Some("foundation"); }
    if has(&["how will we know", "how do we know", "how should it be verified", "acceptance", "success look", "considered done", "tests should", "which tests"]) { return Some("done"); }
    if has(&["what price", "which price", "what color", "what colour", "what label", "what text", "what name", "which name", "what title", "what value", "which value", "what copy", "what hue", "what tag"]) { return Some("values"); }
    if has(&["where should", "where does", "which file", "which component", "which page", "which screen", "which folder", "which one", "which module", "what position", "placement"]) { return Some("target"); }
    if has(&["should return", "should it return", "what should", "should happen", "should it do", "how should", "edge case", "behave", "docstring", "signature", "unspecified", "does not say", "doesn't say", "not specified"]) { return Some("behaviour"); }
    None
}

fn words(s: &str) -> Vec<String> { s.to_ascii_lowercase().split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| w.len() > 1).map(String::from).collect() }
fn norm(s: &str) -> String { words(s).join(" ") }
/// Do two chips say the same thing, or one inside the other? Then keeping both is a contradiction of terms: the operator
/// is offered two answers that are one answer. Token overlap ≥ 70 %, or one contained in the other. Pure.
pub fn overlaps(a: &str, b: &str) -> bool {
    let (na, nb) = (norm(a), norm(b));
    if na.is_empty() || nb.is_empty() { return false }
    if na == nb || (na.len() >= 5 && nb.contains(&na)) || (nb.len() >= 5 && na.contains(&nb)) { return true }
    let (wa, wb) = (words(a), words(b));
    let shared = wa.iter().filter(|w| wb.contains(w)).count();
    shared as f64 / wa.len().max(wb.len()).max(1) as f64 >= 0.7
}
/// Families of choices where the ask, by naming one member, has already answered: a chip naming ANOTHER member of the same
/// family contradicts the ask. Pure data.
const FAMILIES: &[&[&str]] = &[
    &["typescript", "javascript", "python", "rust", "golang", "java", "ruby", "php", "kotlin", "swift", "csharp"],
    &["react", "vue", "svelte", "angular", "solid", "nextjs", "nuxt"],
    &["express", "fastify", "nestjs", "koa", "flask", "django", "fastapi", "rails", "laravel", "spring"],
    &["postgres", "mysql", "sqlite", "mongodb", "redis"],
];
fn mentions(text: &str) -> Vec<String> {
    let t = text.to_ascii_lowercase().replace("c#", "csharp").replace("node.js", "nodejs").replace("next.js", "nextjs").replace("nest.js", "nestjs").replace(" go ", " golang ").replace("postgresql", "postgres");
    words(&t)
}
/// Does this chip contradict something the ask already settled? True when the ask names a member of a family and the chip names a
/// DIFFERENT member and none of the ask's. Pure.
pub fn contradicts_ask(chip: &str, ask: &str) -> bool {
    let (a, c) = (mentions(ask), mentions(chip));
    FAMILIES.iter().any(|fam| {
        let in_ask: Vec<&&str> = fam.iter().filter(|m| a.iter().any(|w| w == **m)).collect();
        if in_ask.is_empty() { return false }
        let in_chip: Vec<&&str> = fam.iter().filter(|m| c.iter().any(|w| w == **m)).collect();
        !in_chip.is_empty() && !in_chip.iter().any(|m| in_ask.contains(m))
    })
}
/// One chip, cleaned; None when it cannot be an answer. Pure.
fn tidy(chip: &str) -> Option<String> {
    let c = chip.trim().trim_end_matches(['.', ';']).trim().to_string();
    if c.is_empty() || c.chars().count() > 90 || c.ends_with('?') { return None }
    let lc = c.to_ascii_lowercase();
    // "A or B" is two answers in one chip; "you decide" is not an answer at all
    if lc.contains(" or ") || lc.contains(" / ") { return None }
    if ["other", "others", "something else", "none of the above", "n/a", "none"].contains(&lc.as_str()) { return None }   // the front end offers "write it yourself"
    if ["antiloki decides", "keep it minimal", "smallest thing", "you decide", "your call", "up to you", "whatever", "no preference", "don't care"].iter().any(|k| lc.contains(k)) { return None }
    Some(c)
}
/// THE CHIPS, HELD TO 3–7 WITHOUT CONTRADICTIONS. `given` are the model's, in its order (the first is its default). Pure — tested.
pub fn chips_for(kind_key: Option<&str>, given: &[String], ask: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut admit = |c: &str, out: &mut Vec<String>| {
        let Some(c) = tidy(c) else { return };
        if contradicts_ask(&c, ask) || out.iter().any(|o| overlaps(o, &c)) { return }
        out.push(c);
    };
    for g in given { admit(g, &mut out); }
    out.truncate(MAX_CHIPS);
    if out.len() < MIN_CHIPS {
        if let Some(k) = kind_key.and_then(kind) { for p in k.pad { if out.len() >= MIN_CHIPS { break } admit(p, &mut out); } }
    }
    out
}

/// THE BOX, AS FACTS FOR THE SCREEN (found live 2026-09-29): a screening call that cannot see the repository asks what the
/// repository answers — "is this a web, desktop or mobile app?" about a React project, with "desktop" as its default. The
/// benches never showed it because a benchmark prompt is self-contained. So the call is handed a short, deterministic sketch of the box:
/// the top-level entries, the source files two levels down, and what package.json depends on. No tools, no exploring — a few hundred
/// characters that also feed `contradicts_ask`, so a chip that names a stack the box does not use is dropped. Empty for an empty box. Pure
/// over the filesystem — tested.
pub fn box_facts(cwd: &str) -> String {
    let root = std::path::Path::new(cwd);
    const SKIP: &[&str] = &["node_modules", ".git", "target", "dist", "build", ".next", ".venv", "__pycache__", ".antiloki", ".cache", "coverage"];
    let list = |dir: &std::path::Path| -> Vec<(String, bool)> {
        let mut v: Vec<(String, bool)> = std::fs::read_dir(dir).map(|rd| rd.filter_map(Result::ok).filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            if SKIP.contains(&n.as_str()) || n.starts_with('.') { return None }
            Some((n, e.path().is_dir()))
        }).collect()).unwrap_or_default();
        v.sort(); v
    };
    let top = list(root);
    if top.is_empty() { return String::new() }
    let mut files: Vec<String> = vec![];
    for (n, is_dir) in top.iter().take(30) {
        if !*is_dir { files.push(n.clone()); continue }
        files.push(format!("{n}/"));
        for (m, sub_dir) in list(&root.join(n)).into_iter().take(12) { files.push(format!("  {n}/{m}{}", if sub_dir { "/" } else { "" })); }
        if files.len() > 70 { break }
    }
    let mut out = format!("files:\n{}", files.join("\n"));
    if let Ok(t) = std::fs::read_to_string(root.join("package.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&t) {
            let mut deps: Vec<String> = vec![];
            for k in ["dependencies", "devDependencies"] { if let Some(o) = v.get(k).and_then(Value::as_object) { deps.extend(o.keys().take(20).cloned()); } }
            if !deps.is_empty() { out.push_str(&format!("\npackage.json depends on: {}", deps.join(", "))); }
        }
    }
    if root.join("Cargo.toml").exists() { out.push_str("\nRust project (Cargo.toml)"); }
    if root.join("pyproject.toml").exists() || root.join("requirements.txt").exists() { out.push_str("\nPython project"); }
    out.chars().take(1800).collect()
}

/// One question, shaped.
#[derive(Clone, Debug, PartialEq)]
pub struct Shaped { pub kind: String, pub text: String, pub chips: Vec<String>, pub default: String }

/// THE DEDICATED SCREEN'S REPLY, HELD TO THE LIST. Accepts `{"questions":[ … ]}` where a question is a plain string (the shape
/// bench/screen-solo first measured) or `{"kind","question"|"text","chips"|"options"}`. Generic questions ("anything in mind?")
/// are dropped; a kind appears at most once; at most three questions. Pure — tested.
pub fn shape_reply(content: &str, ask: &str) -> Vec<Shaped> {
    let (s0, e0) = (content.find('{'), content.rfind('}'));
    let v: Value = match (s0, e0) { (Some(a), Some(b)) if b > a => serde_json::from_str(&content[a..=b]).unwrap_or(Value::Null), _ => Value::Null };
    let mut out: Vec<Shaped> = vec![];
    for q in v.get("questions").and_then(Value::as_array).into_iter().flatten() {
        let text = q.as_str().or_else(|| q.get("question").and_then(Value::as_str)).or_else(|| q.get("text").and_then(Value::as_str)).map(str::trim).unwrap_or("").to_string();
        if text.is_empty() { continue }
        let given: Vec<String> = q.get("chips").or_else(|| q.get("options")).and_then(Value::as_array).map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default();
        if crate::screen::is_generic(&text, &given) { continue }
        // the model's kind if it is on the list; else the words decide; else none
        let named = q.get("kind").and_then(Value::as_str).and_then(kind).map(|k| k.key);
        let key = named.or_else(|| classify(&text));
        if let Some(k) = key { if out.iter().any(|o| o.kind == k) { continue } }
        let chips = chips_for(key, &given, ask);
        let default = chips.first().cloned().unwrap_or_default();
        out.push(Shaped { kind: key.unwrap_or("").to_string(), text, chips, default });
        if out.len() == 3 { break }
    }
    out
}
/// the shaped questions in the planner's own reply shape, so `goals::parse_screen` and the answer round read them unchanged
pub fn as_reply(qs: &[Shaped]) -> String {
    json!({ "questions": qs.iter().map(|q| json!({ "text": q.text, "options": q.chips, "default": q.default, "certainty": "blocking", "kind": q.kind })).collect::<Vec<_>>() }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn s(v: &[&str]) -> Vec<String> { v.iter().map(|x| x.to_string()).collect() }

    #[test]
    fn every_kind_can_reach_the_minimum_by_itself_without_contradicting_itself() {
        for k in KINDS.iter() {
            let chips = chips_for(Some(k.key), &[], "do the thing");
            assert!(chips.len() >= MIN_CHIPS && chips.len() <= MAX_CHIPS, "{}: {chips:?}", k.key);
            for (i, a) in chips.iter().enumerate() { for b in &chips[i + 1..] { assert!(!overlaps(a, b), "{}: {a:?} vs {b:?}", k.key); } }
            assert!(k.frame.contains('<') && !k.frame.is_empty());
        }
        assert!(menu().lines().count() == KINDS.len());
    }

    #[test]
    fn chips_are_three_to_seven_deduplicated_and_never_one_answer_twice() {
        // too many → cut to seven, first kept
        let many = s(&["one thing", "another route", "third path", "fourth way", "fifth idea", "sixth plan", "seventh move", "eighth turn", "ninth step"]);
        let c = chips_for(Some("target"), &many, "x"); assert_eq!(c.len(), 7); assert_eq!(c[0], "one thing");
        // duplicates and containment collapse; then padded from the kind, never below three
        let dup = s(&["Top bar", "top bar.", "the top bar"]);
        let c = chips_for(Some("target"), &dup, "x"); assert!(c.len() >= 3 && c.iter().filter(|x| x.to_lowercase().contains("top bar")).count() == 1, "{c:?}");
        // "A or B", questions and "you decide" are not answers
        let bad = s(&["React or Vue", "Which one?", "you decide", "Sidebar", "Toolbar", "Footer"]);
        assert_eq!(chips_for(Some("target"), &bad, "x"), s(&["Sidebar", "Toolbar", "Footer"]));
    }

    #[test]
    fn a_chip_that_contradicts_what_the_ask_already_settled_is_dropped() {
        let ask = "add a login page in TypeScript with React";
        assert!(contradicts_ask("Python with Flask", ask) && contradicts_ask("Vue with Vite", ask));
        assert!(!contradicts_ask("TypeScript with React (Vite)", ask) && !contradicts_ask("A new file", ask));
        let c = chips_for(Some("foundation"), &s(&["Python with Flask", "Express server", "React with Vite", "Vue app", "Plain HTML"]), ask);
        assert!(!c.iter().any(|x| x.contains("Python") || x.contains("Vue")), "{c:?}");
        assert!(c.len() >= 3);
        // an ask that names no member of a family constrains nothing
        assert!(!contradicts_ask("Python with Flask", "add a login page"));
    }

    #[test]
    fn the_box_is_a_short_factual_sketch_that_also_settles_the_stack() {
        let dir = std::env::temp_dir().join(format!("kinds-box-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap(); std::fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        std::fs::write(dir.join("src/App.tsx"), "x").unwrap(); std::fs::write(dir.join("package.json"), r#"{"name":"shop","dependencies":{"react":"18","vite":"5"}}"#).unwrap();
        let b = box_facts(dir.to_str().unwrap());
        assert!(b.contains("src/App.tsx") && b.contains("package.json depends on: react, vite") && !b.contains("node_modules"), "{b}");
        // the box settles the stack: a chip naming another framework contradicts it
        assert!(contradicts_ask("A Vue single-page app", &format!("add a button\n{b}")));
        assert!(!contradicts_ask("A React component in src", &format!("add a button\n{b}")));
        assert_eq!(box_facts(dir.join("nope").to_str().unwrap()), "", "an empty box has no facts to give");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_and_none_are_not_chips_the_front_end_already_offers_write_it_yourself() {
        let c = chips_for(Some("target"), &s(&["Header", "Other", "Sidebar", "None of the above", "Footer"]), "x");
        assert_eq!(c, s(&["Header", "Sidebar", "Footer"]));
    }

    #[test]
    fn a_question_without_a_kind_is_classified_from_its_words_or_left_bare() {
        assert_eq!(classify("What should candidate(string) return for an empty input?"), Some("behaviour"));
        assert_eq!(classify("Where should the new button go?"), Some("target"));
        assert_eq!(classify("What price and hue should the new product use?"), Some("values"));
        assert_eq!(classify("This repository is empty — which stack should I scaffold?"), Some("foundation"));
        assert_eq!(classify("How will we know it worked?"), Some("done"));
        assert_eq!(classify("This would delete the old table — replace or keep it?"), Some("risk"));
        assert_eq!(classify("Is this what you meant?"), None);
    }

    #[test]
    fn the_reply_is_held_to_the_list_one_question_per_kind_no_generic_ones_at_most_three() {
        let ask = "add a button";
        let reply = r#"{"questions":[
            {"kind":"target","question":"Where should the button go?","chips":["Header","Sidebar","Footer","Toolbar"]},
            {"kind":"target","question":"Which page is it on?","chips":["Home","Settings","Profile"]},
            {"kind":"invented","question":"What should the button do when clicked?","chips":["Open a dialog","Submit the form","Navigate away"]},
            "Anything particular in mind, or shall antiloki decide?",
            {"kind":"values","question":"What label should it show?","chips":["Save"]}]}"#;
        let q = shape_reply(reply, ask);
        assert_eq!(q.len(), 3, "{q:?}");
        assert_eq!(q[0].kind, "target"); assert_eq!(q[0].default, "Header");
        assert_eq!(q[1].kind, "behaviour", "an unknown kind is classified from the words: {q:?}");
        assert_eq!(q[2].kind, "values"); assert!(q[2].chips.len() >= 3 && q[2].chips[0] == "Save", "a short list is padded, the model's own first: {:?}", q[2].chips);
        for x in &q { assert!(x.chips.len() >= MIN_CHIPS && x.chips.len() <= MAX_CHIPS, "{x:?}"); }
        // the plain-string shape (what was benched) still parks the run, bare when nothing fits
        let plain = shape_reply(r#"{"questions":["Is this what you meant exactly?"]}"#, ask);
        assert_eq!(plain.len(), 1); assert!(plain[0].kind.is_empty() && plain[0].chips.is_empty());
        // and the empty reply is "clear"
        assert!(shape_reply(r#"{"questions":[]}"#, ask).is_empty());
        // the reply in the planner's shape carries kind, chips and the default
        let r: Value = serde_json::from_str(&as_reply(&q)).unwrap();
        assert_eq!(r["questions"][0]["kind"], "target"); assert_eq!(r["questions"][0]["default"], "Header");
        assert_eq!(r["questions"][0]["options"].as_array().unwrap().len(), 4);
    }
}
