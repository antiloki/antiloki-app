//! THE CELL FAMILY (RFC 0014; operator 2026-11-16: "a family of specialized nodes: tiny model + code guards that validate, parse or transform"): ONE engine, a registry of TASKS.
//! A task is a prompt and a set of GUARDS written in code: the model proposes, the guards judge (never a model), the exact reason of a refusal goes back ONCE, then a verdict. A new specialized node is a task, not a new engine.
//! Tasks here: `summary` (length, and every number and proper name must come from the source), `translate` (placeholders, numbers, URLs and code spans survive unchanged), `intent` (one label of a list), `redact` (code removes emails, phones and secrets; nothing may remain), `regex` (a pattern that matches every positive example and no negative one).
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const TASKS: [&str; 6] = ["summary", "translate", "intent", "redact", "regex", "match"];
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellSpec {
    /** one of TASKS */ pub task: String,
    /** the tiny model (`local:<tag>`, `endpoint:<alias>`); a task whose code does all the work (redact) may leave it empty */ #[serde(default)] pub seat: String,
    /** where the input text is: `node.field` or `ask` */ pub from: String,
    /** the task's settings: summary {max_words}, translate {to}, intent {labels}, redact {kinds}, regex {positive, negative} */ #[serde(default)] pub params: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub timeout_s: Option<u64>,
}
pub fn check(sp: &CellSpec) -> Vec<String> {
    let mut e = vec![]; if !TASKS.contains(&sp.task.as_str()) { e.push(format!("the task is one of {}", TASKS.join(", "))); }
    if sp.from != "ask" && !sp.from.contains('.') { e.push("`from` is `node.field` (or `ask`)".into()); }
    if sp.seat.is_empty() && sp.task != "redact" { e.push("this task needs a `seat`".into()); } else if !sp.seat.is_empty() { if let Err(x) = crate::team_llm::parse_seat(&sp.seat) { e.push(format!("the seat {x}")); } }
    let list = |k: &str| sp.params.get(k).and_then(Value::as_array).map_or(0, Vec::len);
    match sp.task.as_str() { "intent" => if list("labels") < 2 { e.push("`params.labels` lists at least two labels".into()); }, "translate" => if sp.params.get("to").and_then(Value::as_str).map_or(true, str::is_empty) { e.push("`params.to` is the target language".into()); },
        "match" => { if sp.params.get("labels").and_then(Value::as_object).map_or(0, |m| m.len()) < 2 { e.push("`params.labels` is a map {label: description} of at least two".into()); } if !sp.seat.starts_with("local:") { e.push("`match` needs a local embedding model as its seat (`local:nomic-embed-text`)".into()); } },
        "regex" => if list("positive") == 0 { e.push("`params.positive` lists at least one example the pattern must match".into()); }, _ => {} }
    e
}
fn words(s: &str) -> usize { s.split_whitespace().count() }
fn numbers(s: &str) -> Vec<String> { Regex::new(r"\d+(?:[.,]\d+)?").unwrap().find_iter(s).map(|m| m.as_str().to_string()).collect() }
fn sorted(mut v: Vec<String>) -> Vec<String> { v.sort(); v }
/// Words that start with a capital letter in the MIDDLE of a sentence (a name), lower-cased. Pure.
fn names(s: &str) -> Vec<String> {
    let mut out = vec![]; let mut start = true;
    for w in s.split_whitespace() { let t = w.trim_matches(|c: char| !c.is_alphanumeric()); if !start && t.chars().next().map_or(false, char::is_uppercase) && t.chars().count() > 1 { out.push(t.to_lowercase()); } start = w.ends_with(['.', '!', '?']); }
    out
}
/// Placeholders (`{name}`, `{{name}}`, `%s`, `%d`, `$VAR`), URLs and `code spans` that must survive a translation. Pure.
fn keep_tokens(s: &str) -> Vec<String> { sorted(Regex::new(r"\{\{?[\w.]+\}?\}|%[sd]|\$[A-Z_]+|https?://\S+|`[^`]+`").unwrap().find_iter(s).map(|m| m.as_str().trim_end_matches(['.', ',', ')']).to_string()).collect()) }
const SECRETS: &str = r"(?i)[\w.+-]+@[\w-]+\.[\w.-]+|\+?\d[\d\s().-]{7,}\d|\b(?:sk|pk|ghp|xox[bp])[-_][A-Za-z0-9_-]{16,}|\bAKIA[0-9A-Z]{16}\b|\bbearer\s+[A-Za-z0-9._-]{16,}";
/// Remove what code can recognise: emails, phone numbers, API keys and tokens. Pure.
pub fn redact_text(s: &str) -> String { Regex::new(SECRETS).unwrap().replace_all(s, "[redacted]").to_string() }

pub fn prompt_of(sp: &CellSpec, input: &str) -> String {
    let p = |k: &str| sp.params.get(k).cloned().unwrap_or(Value::Null); let input: String = input.chars().take(8000).collect();
    match sp.task.as_str() {
        "summary" => format!("Summarize the TEXT in at most {} words. Use ONLY facts, numbers and names that are in the text. Reply with the summary only.\\n\\nTEXT:\\n{input}", p("max_words").as_u64().unwrap_or(60)),
        "translate" => format!("Translate the TEXT into {}. Keep every placeholder like {{name}} or %s, every number, every URL and everything in `backticks` EXACTLY as it is. Reply with the translation only.\\n\\nTEXT:\\n{input}", p("to").as_str().unwrap_or("English")),
        "intent" => format!("Which one of these labels fits the TEXT best: {}? Reply with ONLY the label.\\n\\nTEXT:\\n{input}", p("labels").as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default()),
        "redact" => format!("Replace every person's name in the TEXT with [name]. Change nothing else. Reply with the text only.\\n\\nTEXT:\\n{input}"),
        "regex" => format!("Write ONE regular expression (Rust regex syntax) that matches every line of POSITIVE and no line of NEGATIVE. Reply with ONLY the pattern.\\n\\nPOSITIVE:\\n{}\\nNEGATIVE:\\n{}", p("positive").as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\\n")).unwrap_or_default(), p("negative").as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\\n")).unwrap_or_default()),
        _ => input,
    }.replace("\\\\n", "\n")
}
/// How many tokens a task may ask for: a label is short, a translation is as long as its text. Pure.
pub fn max_tokens(task: &str) -> u32 { match task { "intent" => 80, "regex" => 200, "summary" => 400, _ => 900 } }
/// A Qwen3 model thinks out loud unless told `/no_think` (its own switch): the prompt carries it for those seats. Pure.
pub fn for_seat(seat: &str, prompt: String) -> String { if seat.to_lowercase().contains("qwen3") && !prompt.contains("/no_think") { format!("{prompt}\n/no_think") } else { prompt } }
/// The GUARDS: the reply of the model against the input, by code. Ok(the clean output) or the exact reason it is refused. Pure.
pub fn guard(sp: &CellSpec, input: &str, reply: &str) -> Result<Value, String> {
    let r = crate::team_decompose::strip_think(reply); let r = r.trim().trim_matches('`').trim().to_string(); if r.is_empty() { return Err("the reply is empty".into()); }
    match sp.task.as_str() {
        "summary" => {
            let max = sp.params.get("max_words").and_then(Value::as_u64).unwrap_or(60) as usize; if words(&r) > max { return Err(format!("{} words, at most {max}", words(&r))); }
            let src_n = numbers(input); if let Some(n) = numbers(&r).into_iter().find(|n| !src_n.contains(n)) { return Err(format!("the number {n} is not in the text")); }
            let src_names: Vec<String> = names(input).into_iter().chain(input.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())).collect(); if let Some(n) = names(&r).into_iter().find(|n| !src_names.contains(n)) { return Err(format!("the name {n} is not in the text")); }
            Ok(json!(r))
        }
        "translate" => {
            let (a, b) = (keep_tokens(input), keep_tokens(&r)); if a != b { let lost: Vec<&String> = a.iter().filter(|x| !b.contains(x)).collect(); return Err(format!("these must survive unchanged: {}", if lost.is_empty() { "no extra ones may appear".to_string() } else { lost.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ") })); }
            if sorted(numbers(input)) != sorted(numbers(&r)) { return Err("the numbers changed".into()); } if r.trim() == input.trim() && words(input) > 3 { return Err("the text was not translated".into()); }
            Ok(json!(r))
        }
        "intent" => {
            let labels: Vec<String> = sp.params.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default();
            let norm = |s: &str| s.trim().trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            // the bare label on the first line is the answer; a model that talks first is read for the ONE label it names (the last line first)
            let first = norm(r.lines().next().unwrap_or(""));
            if let Some(l) = labels.iter().find(|l| l.to_lowercase() == first) { return Ok(json!(l)); }
            let mentions = |text: &str| -> Vec<&String> { let words: Vec<String> = text.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_').map(|w| w.to_lowercase()).collect(); labels.iter().filter(|l| words.iter().any(|w| *w == l.to_lowercase())).collect() };
            let last = r.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
            let (a, b) = (mentions(last), mentions(&r));
            if a.len() == 1 { return Ok(json!(a[0])); } if b.len() == 1 { return Ok(json!(b[0])); }
            Err(if b.is_empty() { format!("no label of this list is in the reply: {}", labels.join(", ")) } else { format!("the reply names several labels ({}): answer with ONE of: {}", b.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "), labels.join(", ")) })
        }
        "redact" => { let out = redact_text(&r); if Regex::new(SECRETS).unwrap().is_match(&out) { return Err("something sensitive is still there".into()); } let base = redact_text(input); if out.len() * 2 + 40 < base.len() { return Err("too much of the text was removed".into()); } Ok(json!(out)) }
        "regex" => {
            let pat = r.lines().next().unwrap_or("").trim().trim_matches('/').to_string(); let re = Regex::new(&pat).map_err(|e| format!("`{pat}` is not a valid pattern: {e}"))?;
            let list = |k: &str| sp.params.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>()).unwrap_or_default();
            if let Some(p) = list("positive").iter().find(|p| !re.is_match(p)) { return Err(format!("it does not match `{p}`")); } if let Some(n) = list("negative").iter().find(|n| re.is_match(n)) { return Err(format!("it matches `{n}` and must not")); }
            Ok(json!(pat))
        }
        _ => Err("unknown task".into()),
    }
}
/// Cosine similarity of two vectors. Pure.
pub fn cosine(a: &[f64], b: &[f64]) -> f64 { let (mut d, mut x, mut y) = (0.0, 0.0, 0.0); for (p, q) in a.iter().zip(b) { d += p * q; x += p * p; y += q * q; } if x == 0.0 || y == 0.0 { 0.0 } else { d / (x.sqrt() * y.sqrt()) } }
/// The label whose description is closest to the input, with its score and the margin over the runner-up. Pure.
pub fn best_match(input: &[f64], labels: &[(String, Vec<f64>)]) -> Option<(String, f64, f64)> {
    let mut s: Vec<(f64, &String)> = labels.iter().map(|(l, v)| (cosine(input, v), l)).collect(); s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let first = s.first()?; Some((first.1.clone(), first.0, first.0 - s.get(1).map_or(0.0, |x| x.0)))
}
/// What a run did.
#[derive(Clone, Debug, PartialEq)]
pub struct Done { pub output: Value, pub tries: u32, pub usd: f64 }
/// The cell: the model proposes, the guards judge, the exact reason goes back once. `redact` is code first: the model is asked only for names, if a seat is set.
pub async fn run(sp: &CellSpec, input: &str, lctx: Option<crate::ledger::Ctx>) -> Result<Done, String> {
    let e = check(sp); if !e.is_empty() { return Err(e.join("; ")); }
    // MATCH (VID-19): no generation at all. The input and each label's description are embedded; the closest description wins. `min_score` (with `default`) keeps a weak match from being forced.
    if sp.task == "match" {
        let labels: Vec<(String, String)> = sp.params["labels"].as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or(k).to_string())).collect()).unwrap_or_default();
        let tag = sp.seat.strip_prefix("local:").unwrap_or(""); let mut texts = vec![input.to_string()]; texts.extend(labels.iter().map(|(_, d)| d.clone()));
        let v = crate::team_llm::embed(tag, &texts, sp.timeout_s.unwrap_or(60)).await?;
        let (best, score, _margin) = best_match(&v[0], &labels.iter().map(|(l, _)| l.clone()).zip(v[1..].iter().cloned()).collect::<Vec<_>>()).ok_or("no labels")?;
        let pick = match (sp.params.get("min_score").and_then(Value::as_f64), sp.params.get("default").and_then(Value::as_str)) { (Some(min), Some(d)) if score < min => d.to_string(), _ => best };
        return Ok(Done { output: json!(pick), tries: 1, usd: 0.0 });
    }
    let (mut base, mut usd) = (input.to_string(), 0.0);
    if sp.task == "redact" { base = redact_text(input); if sp.seat.is_empty() { return guard(sp, input, &base).map(|o| Done { output: o, tries: 0, usd: 0.0 }); } }
    let seat = crate::team_llm::parse_seat(&sp.seat)?; let t = sp.timeout_s.unwrap_or(120); let mut asked = for_seat(&sp.seat, prompt_of(sp, &base)); let mut last = String::new();
    for round in 0..2 {
        let (txt, c, _) = match crate::team_llm::call_plain(&seat, &asked, max_tokens(&sp.task), t, lctx.clone()).await { Ok(x) => x, Err(e) if round == 0 => { last = e; continue }, Err(e) => return Err(e) }; usd += c;   // a slow or dropped call is asked once more
        match guard(sp, if sp.task == "redact" { &base } else { input }, &txt) { Ok(o) => return Ok(Done { output: o, tries: round + 1, usd }), Err(e) => { last = e.clone(); asked = for_seat(&sp.seat, format!("{}\n\nYour last reply was refused: {e}. Fix exactly that.", prompt_of(sp, &base))); } }
    }
    Err(format!("the guards refused the model's reply twice: {last}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(task: &str, params: Value) -> CellSpec { CellSpec { task: task.into(), seat: "endpoint:lfm3".into(), from: "ask".into(), params: params.as_object().cloned().unwrap_or_default().into_iter().collect(), timeout_s: None } }
    #[test] fn a_summary_is_held_to_its_length_and_to_the_source() {
        let src = "Maria Silva paid 120 dollars to Acme on 3 March for the new server.";
        let s = spec("summary", json!({ "max_words": 8 }));
        assert_eq!(guard(&s, src, "Maria paid 120 dollars to Acme.").unwrap(), json!("Maria paid 120 dollars to Acme."));
        assert!(guard(&s, src, "Maria paid 130 dollars to Acme.").unwrap_err().contains("number 130")); assert!(guard(&s, src, "Maria paid Google 120 dollars.").unwrap_err().contains("name google")); assert!(guard(&s, src, "Maria Silva paid 120 dollars to Acme on 3 March for the new server today").unwrap_err().contains("at most 8"));
        assert!(guard(&s, src, "  ").is_err());
    }
    #[test] fn a_translation_keeps_placeholders_numbers_urls_and_code() {
        let t = spec("translate", json!({ "to": "Portuguese" })); let src = "Hello {name}, you have 3 files. See https://a.dev/x and run `npm test`.";
        assert!(guard(&t, src, "Olá {name}, você tem 3 arquivos. Veja https://a.dev/x e rode `npm test`.").is_ok());
        assert!(guard(&t, src, "Olá {nome}, você tem 3 arquivos. Veja https://a.dev/x e rode `npm test`.").unwrap_err().contains("{name}")); assert!(guard(&t, src, "Olá {name}, você tem 4 arquivos. Veja https://a.dev/x e rode `npm test`.").unwrap_err().contains("numbers"));
        assert!(guard(&t, src, src).unwrap_err().contains("not translated"));
    }
    #[test] fn an_intent_is_one_of_the_labels_and_nothing_else() {
        let i = spec("intent", json!({ "labels": ["bug", "feature", "question"] })); assert_eq!(guard(&i, "x", "Feature.").unwrap(), json!("feature")); assert_eq!(guard(&i, "x", "<think></think>question\nbecause").unwrap(), json!("question")); assert!(guard(&i, "x", "complaint").unwrap_err().contains("no label of this list")); assert_eq!(guard(&i, "x", "We are given a text about a button. The best label is: bug").unwrap(), json!("bug"), "a model that talks first is read for the one label it names"); assert!(guard(&i, "x", "It could be a bug or a feature.").unwrap_err().contains("several labels"));
    }
    #[test] fn redaction_is_code_and_nothing_sensitive_may_remain() {
        let src = "Mail ana@corp.com or +55 11 91234-5678, key sk-abcdefghijklmnop1234, aws AKIAABCDEFGHIJKLMNOP."; let out = redact_text(src);
        assert!(!out.contains("ana@corp.com") && !out.contains("91234") && !out.contains("sk-abc") && !out.contains("AKIA") && out.contains("[redacted]"), "{out}");
        assert!(guard(&spec("redact", json!({})), src, src).is_ok(), "the guard redacts then re-scans"); assert!(guard(&spec("redact", json!({})), "short ana@corp.com and a long text that goes on and on and on and on and on and on and on and on", "x").is_err());
    }
    #[test] fn a_regex_must_match_every_positive_and_no_negative() {
        let r = spec("regex", json!({ "positive": ["a1", "b22"], "negative": ["c", "1a"] })); assert_eq!(guard(&r, "", "^[ab]\\d+$").unwrap(), json!("^[ab]\\d+$"));
        assert!(guard(&r, "", "^a\\d$").unwrap_err().contains("does not match `b22`")); assert!(guard(&r, "", "\\w+").unwrap_err().contains("matches `c`")); assert!(guard(&r, "", "([").unwrap_err().contains("not a valid pattern"));
    }
    #[test] fn a_qwen3_seat_is_told_not_to_think_and_a_task_has_a_token_cap() {
        assert!(for_seat("local:qwen3:4b", "hi".into()).ends_with("/no_think")); assert_eq!(for_seat("local:qwen2.5-coder:3b", "hi".into()), "hi"); assert_eq!(for_seat("local:qwen3:4b", "x\n/no_think".into()), "x\n/no_think");
        assert!(max_tokens("intent") < max_tokens("summary") && max_tokens("summary") < max_tokens("translate"));
    }
    #[test] fn a_spec_says_what_is_wrong() {
        assert!(check(&spec("intent", json!({ "labels": ["a", "b"] }))).is_empty()); assert!(!check(&spec("intent", json!({ "labels": ["a"] }))).is_empty()); assert!(!check(&spec("magic", json!({}))).is_empty());
        assert!(check(&CellSpec { seat: String::new(), ..spec("redact", json!({})) }).is_empty()); assert!(!check(&CellSpec { seat: String::new(), ..spec("summary", json!({})) }).is_empty());
    }
    #[test] fn a_match_cell_picks_the_closest_description_by_vectors() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-9 && cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-9 && cosine(&[0.0, 0.0], &[1.0, 0.0]) == 0.0);
        let labels = vec![("sea".to_string(), vec![0.0, 1.0, 0.0]), ("desert".to_string(), vec![1.0, 0.0, 0.0]), ("garden".to_string(), vec![0.0, 0.0, 1.0])];
        let (l, s, m) = best_match(&[0.9, 0.2, 0.0], &labels).unwrap(); assert_eq!(l, "desert"); assert!(s > 0.9 && m > 0.5);
        let mut sp = spec("match", json!({ "labels": { "a": "x", "b": "y" } })); sp.seat = "local:nomic-embed-text".into(); assert!(check(&sp).is_empty());
        sp.seat = "claude-code:haiku".into(); assert!(!check(&sp).is_empty(), "a match needs a local embedding seat");
        let mut one = spec("match", json!({ "labels": { "a": "x" } })); one.seat = "local:nomic-embed-text".into(); assert!(!check(&one).is_empty());
    }
}
