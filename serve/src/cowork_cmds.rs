//! THE SCREEN'S COMMAND CHANNEL (RFC 0007 §17.10). Only the screen can open a pane that a person can see, so when an agent's MCP tool asks for one ("open a terminal and run the tests",
//! "show the preview") the serve cannot do it alone: it parks a COMMAND here, the open screen takes it, does it in the layout it owns, and reports what it made. If no screen is open
//! the answer is immediate and honest ("no screen is open"), never a hang and never a pane nobody sees.
//!
//!   GET  /api/v1/cockpit/cowork/commands          the commands waiting for the screen (and the proof that a screen is watching)
//!   POST /api/v1/cockpit/cowork/commands/result   `{id, ok, ...}` the screen's report
//!   POST /api/v1/cockpit/cowork/commands/run      `{op, args, wait_ms?}` enqueue, wait for the report (what the MCP door uses)
//!
//! Nothing is persisted: a command lives for a few seconds. Pure over a store; the handlers (in `cowork_mcp.rs`) only find the clock and the request.
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

/// A command nobody took in this long is dropped (the screen was closed, or never came).
pub const TTL_MS: i64 = 45_000;
/// A screen counts as watching when it asked for commands this recently.
pub const SCREEN_RECENT_MS: i64 = 6_000;
/// What a command may ask the screen to do. Anything else is refused before it is queued.
pub const OPS: [&str; 10] = ["open", "close", "focus", "arrange", "look", "screen", "pane_view", "pane_act", "specialist_edit", "connector_open"];

#[derive(Debug, Clone)]
pub struct Cmd { pub id: u64, pub op: String, pub args: Value, pub created: i64, pub result: Option<Value>, /** when a screen took it */ pub taken: i64 }
/// How long the screen that took a command has to report before another is offered it.
const CLAIM_MS: i64 = 15_000;
#[derive(Default)]
pub struct Store { next: u64, items: Vec<Cmd>, screen_seen: i64 }

impl Store {
    pub fn enqueue(&mut self, op: &str, args: &Value, now: i64) -> Result<u64, String> {
        if !OPS.contains(&op) { return Err(format!("the screen does not do `{op}` (it does: {})", OPS.join(", "))); }
        self.items.retain(|c| now - c.created < TTL_MS * 4);
        self.next += 1; self.items.push(Cmd { id: self.next, op: op.to_string(), args: args.clone(), created: now, result: None, taken: 0 });
        Ok(self.next)
    }
    /// What the screen should do now. Asking is what tells the serve a screen is open. A command goes to ONE screen (found 2026-11-26: with two screens open, one "open" made two panes); it is offered again only
    /// when whoever took it has not reported for `CLAIM_MS`.
    pub fn pending(&mut self, now: i64) -> Vec<Value> {
        self.screen_seen = now;
        self.items.iter_mut().filter(|c| c.result.is_none() && now - c.created < TTL_MS && (c.taken == 0 || now - c.taken >= CLAIM_MS)).map(|c| { c.taken = now; json!({ "id": c.id, "op": c.op, "args": c.args }) }).collect()
    }
    pub fn screen_watching(&self, now: i64) -> bool { self.screen_seen > 0 && now - self.screen_seen < SCREEN_RECENT_MS }
    pub fn report(&mut self, id: u64, result: &Value) -> Result<(), String> {
        let c = self.items.iter_mut().find(|c| c.id == id).ok_or_else(|| "that command is gone".to_string())?;
        if c.result.is_none() { c.result = Some(result.clone()); }
        Ok(())
    }
    /// The report, once (it is taken off the store).
    pub fn take_result(&mut self, id: u64) -> Option<Value> {
        let i = self.items.iter().position(|c| c.id == id && c.result.is_some())?;
        self.items.remove(i).result
    }
    pub fn forget(&mut self, id: u64) { self.items.retain(|c| c.id != id); }
}

fn store() -> &'static Mutex<Store> { static S: OnceLock<Mutex<Store>> = OnceLock::new(); S.get_or_init(Default::default) }
pub fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R { f(&mut store().lock().unwrap_or_else(|p| p.into_inner())) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_offered_until_the_screen_reports_and_the_report_is_handed_over_once() {
        let mut s = Store::default();
        let id = s.enqueue("open", &json!({ "kind": "terminal", "command": "npm test" }), 1_000).unwrap();
        let p = s.pending(1_100); assert_eq!(p.len(), 1); assert_eq!((p[0]["id"].clone(), p[0]["op"].clone(), p[0]["args"]["command"].clone()), (json!(id), json!("open"), json!("npm test")));
        assert!(s.pending(1_200).is_empty(), "taken: a second screen is not given the same command");
        assert_eq!(s.pending(1_100 + CLAIM_MS).len(), 1, "offered again when whoever took it never reported");
        assert!(s.take_result(id).is_none(), "nothing to hand over before the screen reports");
        s.report(id, &json!({ "ok": true, "pane_id": 12 })).unwrap();
        assert!(s.pending(1_300 + 2 * CLAIM_MS).is_empty(), "a reported command is not offered again");
        assert_eq!(s.take_result(id).unwrap()["pane_id"], 12); assert!(s.take_result(id).is_none(), "once");
        assert!(s.report(id, &json!({})).is_err(), "a report for something gone says so");
    }

    #[test]
    fn only_what_the_screen_does_is_queued_and_a_command_nobody_took_goes_stale() {
        let mut s = Store::default();
        let e = s.enqueue("rm -rf", &json!({}), 0).unwrap_err(); assert!(e.contains("does not do"), "{e}");
        for op in OPS { assert!(s.enqueue(op, &json!({}), 0).is_ok(), "{op}"); }
        assert!(s.pending(TTL_MS + 1).is_empty(), "stale");
    }

    #[test]
    fn a_screen_is_watching_only_while_it_keeps_asking() {
        let mut s = Store::default();
        assert!(!s.screen_watching(1_000), "no screen has ever asked");
        s.pending(1_000); assert!(s.screen_watching(1_000 + SCREEN_RECENT_MS - 1)); assert!(!s.screen_watching(1_000 + SCREEN_RECENT_MS));
    }
}
