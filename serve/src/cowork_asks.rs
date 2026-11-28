//! THE PRE-CHECK'S QUESTIONS, ASKED OF THE PERSON (RFC 0007 §17.6). The engine's own prompt hook (`antiloki-agent hook`) asks the pre-check about what was typed straight into a
//! terminal. When the ask has a real question (a named file that is not there) and a screen is open to show it, the serve PARKS the question here, the screen shows the floating
//! card over that pane, and the hook waits for the answer (a long poll) and hands it to the model with the prompt. A hook cannot rewrite the prompt, so the answer travels as context.
//!
//!   POST /api/v1/cockpit/precheck            (source "hook") a question with a screen watching: `{verdict:"ask", ask_id, waiting:true}`, parked here
//!   GET  /api/v1/cockpit/cowork/asks         the open questions, for the screen (and the proof that a screen is watching)
//!   POST /api/v1/cockpit/cowork/asks/answer  `{id, answers:{<question id>: <option>}}` or `{id, skip:true}`
//!   POST /api/v1/cockpit/cowork/asks/wait    `{id}` the hook's long poll: the person's answer, a dismissal, or a timeout
//!
//! Nothing here is persisted: an open question lives for the length of one wait. Pure over a store; the handlers (in `cowork.rs`) only find the clock and the request.
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

/// How long a question stays open for the person, and so how long the hook waits.
pub const WAIT_MS: i64 = 60_000;
/// A screen counts as watching when it asked for the open questions this recently.
pub const SCREEN_RECENT_MS: i64 = 6_000;

#[derive(Debug, Clone)]
pub struct Pending { pub id: u64, pub pane_id: i64, pub text: String, pub questions: Value, pub notes: Value, pub created: i64, pub answer: Option<Value> }
#[derive(Default)]
pub struct Store { next: u64, items: Vec<Pending>, screen_seen: i64 }

impl Store {
    /// Park a question. At most one per pane: a newer ask from the same pane replaces the older (the person has moved on).
    pub fn register(&mut self, pane_id: i64, text: &str, questions: &Value, notes: &Value, now: i64) -> u64 {
        self.items.retain(|p| p.pane_id != pane_id);
        self.next += 1;
        self.items.push(Pending { id: self.next, pane_id, text: text.chars().take(2000).collect(), questions: questions.clone(), notes: notes.clone(), created: now, answer: None });
        self.next
    }
    /// The questions the screen should show now. Asking is what tells the serve a screen is open.
    pub fn open(&mut self, now: i64) -> Vec<Value> {
        self.screen_seen = now;
        self.items.retain(|p| now - p.created < WAIT_MS + 5_000);
        self.items.iter().filter(|p| p.answer.is_none()).map(|p| json!({ "id": p.id, "pane_id": p.pane_id, "text": p.text, "questions": p.questions, "notes": p.notes, "created": p.created })).collect()
    }
    pub fn screen_watching(&self, now: i64) -> bool { self.screen_seen > 0 && now - self.screen_seen < SCREEN_RECENT_MS }
    /// Record the person's answer. `answers` maps a question id to the chosen option; a question left out was deferred. `skip` dismisses the whole card.
    pub fn answer(&mut self, id: u64, body: &Value) -> Result<(), String> {
        let p = self.items.iter_mut().find(|p| p.id == id).ok_or_else(|| "that question is not open any more".to_string())?;
        if p.answer.is_some() { return Ok(()); }
        p.answer = Some(if body["skip"] == true { json!({ "skip": true }) } else { json!({ "answers": body["answers"].clone() }) });
        Ok(())
    }
    /// The hook's side: the answer if there is one (and the question is taken off the store), else `None` while it is still open.
    pub fn take_answer(&mut self, id: u64) -> Option<Value> {
        let i = self.items.iter().position(|p| p.id == id && p.answer.is_some())?;
        let p = self.items.remove(i);
        Some(verdict_of(&p))
    }
    pub fn forget(&mut self, id: u64) { self.items.retain(|p| p.id != id); }
    /// Nobody answered in time: take the question off the screen and hand it back AS A QUESTION (the model will ask the person itself), marked as timed out.
    pub fn expire(&mut self, id: u64) -> Option<Value> {
        let i = self.items.iter().position(|p| p.id == id)?;
        let p = self.items.remove(i);
        Some(json!({ "verdict": "ask", "tier": 0, "questions": p.questions, "notes": p.notes, "timed_out": true }))
    }
    pub fn exists(&self, id: u64) -> bool { self.items.iter().any(|p| p.id == id) }
}

/// What the hook is told once the person has answered: `answered` with each question and the option chosen, or `send` when the card was dismissed. Pure.
pub fn verdict_of(p: &Pending) -> Value {
    let a = p.answer.clone().unwrap_or(Value::Null);
    if a["skip"] == true { return json!({ "verdict": "send", "tier": 0, "notes": ["the person dismissed the pre-check"], "by": "person" }); }
    let chosen: Vec<Value> = p.questions.as_array().into_iter().flatten().filter_map(|q| {
        let qid = q["id"].as_str()?; let opt = a["answers"][qid].as_str()?;
        Some(json!({ "question": q["text"], "answer": opt }))
    }).collect();
    if chosen.is_empty() { return json!({ "verdict": "send", "tier": 0, "notes": ["the person answered none of the questions"], "by": "person" }); }
    json!({ "verdict": "answered", "tier": 0, "answers": chosen, "by": "person" })
}

fn store() -> &'static Mutex<Store> { static S: OnceLock<Mutex<Store>> = OnceLock::new(); S.get_or_init(Default::default) }
pub fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R { f(&mut store().lock().unwrap_or_else(|p| p.into_inner())) }

#[cfg(test)]
mod tests {
    use super::*;

    fn qs() -> Value { json!([{ "id": "q1", "text": "`src/x.tsx` is not in this box", "options": ["create it", "another path"] }, { "id": "q2", "text": "which folder?", "options": ["src", "lib"] }]) }

    #[test]
    fn a_parked_question_is_open_until_answered_and_the_answer_is_handed_over_once() {
        let mut s = Store::default();
        let id = s.register(7, "fix x", &qs(), &json!([]), 1_000);
        let open = s.open(1_500); assert_eq!(open.len(), 1); assert_eq!((open[0]["id"].clone(), open[0]["pane_id"].clone()), (json!(id), json!(7)));
        assert!(s.take_answer(id).is_none(), "nothing to hand over while the person has not answered");
        s.answer(id, &json!({ "answers": { "q1": "create it" } })).unwrap();
        assert!(s.open(2_000).is_empty(), "an answered question is not shown again");
        let v = s.take_answer(id).unwrap();
        assert_eq!(v["verdict"], "answered"); assert_eq!(v["answers"], json!([{ "question": "`src/x.tsx` is not in this box", "answer": "create it" }]), "a deferred question is left out");
        assert!(s.take_answer(id).is_none() && !s.exists(id), "taken once, then gone");
        assert!(s.answer(id, &json!({})).is_err(), "answering something gone says so");
    }

    #[test]
    fn a_question_nobody_answered_goes_back_as_a_question_with_its_options() {
        let mut s = Store::default();
        let id = s.register(2, "t", &qs(), &json!(["reads as: a lookup"]), 0);
        let v = s.expire(id).unwrap();
        assert_eq!((v["verdict"].clone(), v["timed_out"].clone(), v["questions"][0]["id"].clone()), (json!("ask"), json!(true), json!("q1")));
        assert!(!s.exists(id) && s.expire(id).is_none(), "gone from the screen, and only once");
    }

    #[test]
    fn a_dismissal_and_an_empty_answer_both_let_the_prompt_through() {
        let mut s = Store::default();
        let a = s.register(1, "t", &qs(), &json!([]), 0); s.answer(a, &json!({ "skip": true })).unwrap();
        assert_eq!(s.take_answer(a).unwrap()["verdict"], "send");
        let b = s.register(1, "t", &qs(), &json!([]), 0); s.answer(b, &json!({ "answers": {} })).unwrap();
        let v = s.take_answer(b).unwrap(); assert_eq!(v["verdict"], "send"); assert!(v["notes"][0].as_str().unwrap().contains("none of the questions"));
        let c = s.register(1, "t", &qs(), &json!([]), 0); s.answer(c, &json!({ "answers": { "q1": "create it", "q2": "lib" } })).unwrap();
        assert_eq!(s.take_answer(c).unwrap()["answers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_newer_ask_from_the_same_pane_replaces_the_older_and_old_ones_expire() {
        let mut s = Store::default();
        let a = s.register(3, "first", &qs(), &json!([]), 0);
        let b = s.register(3, "second", &qs(), &json!([]), 10); assert_ne!(a, b); assert!(!s.exists(a) && s.exists(b));
        s.register(4, "other pane", &qs(), &json!([]), 10); assert_eq!(s.open(20).len(), 2);
        assert!(s.open(10 + WAIT_MS + 6_000).is_empty(), "a question nobody answered does not stay for ever");
    }

    #[test]
    fn a_screen_is_watching_only_while_it_keeps_asking() {
        let mut s = Store::default();
        assert!(!s.screen_watching(1_000), "no screen has ever asked");
        s.open(1_000); assert!(s.screen_watching(1_000 + SCREEN_RECENT_MS - 1)); assert!(!s.screen_watching(1_000 + SCREEN_RECENT_MS));
    }
}
