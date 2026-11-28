//! interviewer-topic-anchor.rs — twin of skills/agents/in/interviewer-topic-anchor.mjs (db READ lane).
//! NAMES ARE CONTRACT: `interviewerTopicAnchor` (default) · isShortFollowUp (no-db). Prev-turn subject anchor.
// QUALITY: 79/100 — look back ≤3 turns for a plan naming a plausible slug; short-follow-up heuristic
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn slug_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"^[A-Z][A-Z0-9-]+$").unwrap()) }
fn is_plausible_slug(v: &Value) -> bool { v.as_str().map_or(false, |s| s.chars().count() > 2 && slug_re().is_match(s)) }

const MAX_SAFE_INTEGER: i64 = 9007199254740991;

pub fn interviewerTopicAnchor(input: &Value, db: &Connection) -> Result<Value, String> {
    let none = || json!({ "slug": null, "action": null, "kind": null, "prompt_block_md": "(none)" });
    let session_id = match nz(input.get("session_id")) { Some(v) => v.clone(), None => return Ok(none()) };
    let ti: i64 = match input.get("current_turn_index") { Some(Value::Number(n)) => n.as_f64().map(|f| f as i64).unwrap_or(MAX_SAFE_INTEGER), _ => MAX_SAFE_INTEGER };

    let prev = crate::db_json::query_rows(db,
        "SELECT turn_index, user_text, classified_kind, proposed_actions_json FROM interviewer_turns WHERE session_id = ? AND turn_index < ? ORDER BY turn_index DESC LIMIT 3",
        &[session_id, json!(ti)]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();

    for row in &prev {
        let plan: Value = row.get("proposed_actions_json").and_then(|v| v.as_str())
            .map(|s| serde_json::from_str::<Value>(s).unwrap_or(json!([]))).unwrap_or(json!([]));
        let plan = match plan.as_array() { Some(a) if !a.is_empty() => a, _ => continue };
        let first = &plan[0];
        let action = first.get("action").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
        let args = first.get("args").filter(|v| !v.is_null()).cloned().unwrap_or(json!({}));
        let slug_candidate = nz(args.get("slug")).or_else(|| nz(args.get("card_slug"))).or_else(|| nz(args.get("parent_slug")));
        if slug_candidate.map_or(false, is_plausible_slug) {
            let slug = slug_candidate.unwrap();
            let kind = row.get("classified_kind").cloned().unwrap_or(Value::Null);
            let action_disp = if action.is_null() { "(none)".to_string() } else { str_qq(Some(&action)) };
            let user_said: String = str_qq(row.get("user_text")).chars().take(120).collect();
            let block = format!("slug: {}\nprevious action: {}\nprevious kind: {}\nprevious user said: {}",
                str_qq(Some(slug)), action_disp, tmpl(row.get("classified_kind")), user_said);
            return Ok(json!({ "slug": slug, "action": action, "kind": kind, "prompt_block_md": block }));
        }
    }
    Ok(none())
}

pub fn isShortFollowUp(user_text: &Value) -> Value {
    let s = match user_text.as_str() { Some(x) => x, None => return json!(false) };
    let t = s.trim();
    if t.is_empty() || t.chars().count() > 80 { return json!(false); }
    let word_count = t.split_whitespace().count();
    if word_count > 8 { return json!(false); }
    static CONT: OnceLock<Regex> = OnceLock::new();
    let cont = CONT.get_or_init(|| Regex::new(r"(?i)^(and|but|what about|how about|now|then|also|or|so|show it|tell me about)\b").unwrap());
    let ends_q = t.ends_with('?');
    json!(cont.is_match(t) || (ends_q && word_count <= 6))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anchors_and_followups() {
        assert_eq!(isShortFollowUp(&json!("and the contract?")), json!(true));
        assert_eq!(isShortFollowUp(&json!("show it")), json!(true));
        assert_eq!(isShortFollowUp(&json!("please write a fully detailed multi-paragraph specification now")), json!(false)); // >8 words
        assert_eq!(isShortFollowUp(&json!("android app")), json!(false)); // 'and' needs a word boundary
        assert_eq!(isShortFollowUp(&json!(42)), json!(false));

        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE interviewer_turns (session_id TEXT, turn_index INTEGER, user_text TEXT, classified_kind TEXT, proposed_actions_json TEXT); \
          INSERT INTO interviewer_turns VALUES ('s1', 0, 'add the FEAT-X card', 'mutate', '[{\"action\":\"card.add\",\"args\":{\"slug\":\"FEAT-X\"}}]');").unwrap();
        let r = interviewerTopicAnchor(&json!({ "session_id": "s1", "current_turn_index": 1 }), &c).unwrap();
        assert_eq!(r["slug"], json!("FEAT-X"));
        assert_eq!(r["kind"], json!("mutate"));
        assert!(r["prompt_block_md"].as_str().unwrap().contains("slug: FEAT-X"));
        assert_eq!(interviewerTopicAnchor(&json!({}), &c).unwrap()["prompt_block_md"], json!("(none)"));
    }
}
