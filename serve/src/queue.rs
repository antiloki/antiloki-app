//! THE QUEUE - every agent's list of what was asked: done, doing, next (operator 2026-09-29: "every agent should have its own queue ... the things asked like a
//! todolist that will be done or was done ... it is Heimdall's router, but it already chooses on the queue, and when that task is next it is already ready").
//!
//! WHAT IT IS. One ordered list per pane (an agent). Each message the operator sends that is a TASK is added as an ITEM: the consolidated goal, what it names (paths), what it
//! asks (create / remove / undo), and - decided NOW, not at execution - who will do it: the role and the merge (planner x worker) Heimdall's router picks. Items run one at a
//! time per agent, in order; a run that is waiting on the operator's answer or approval still holds its turn (the next item must not start on a box someone is deciding about).
//! The operator can cancel a queued item, drag it to another place, re-prompt it (its binding is re-decided) or, for a finished one, undo it (put the box back to its checkpoint).
//!
//! WHY IT MAKES FEWER MISTAKES, and where it could make more. Two runs no longer edit one box at once. Nothing is "started by a stray go": an item is either queued, doing, or
//! settled, and the panel says which. The route is visible BEFORE the work begins, so a wrong one is dragged out or re-prompted instead of discovered in the report. The danger is
//! a binding decided early going stale (an earlier item moves the measured record, changes the files, signs a seat out): so the binding is a PROPOSAL until the item's turn, and at
//! that moment code re-decides it (`tick` -> `bind`) and, when it differs, says so on the item ("re-bound: A -> B") instead of running the stale one. No model is asked to keep the list.
//!
//! WHY A MODEL CANNOT IGNORE IT. The list is written by code, checkpointed by code (`checkpoint::take` with the list inside the commit message), and put in front of every model
//! that touches the box (`state_block`: planner, worker, verifier, QA judge, the chat turn). What the list says about the box is enforced, not requested: see `checkpoint::gate`.
use crate::checkpoint::Checkpoint;
use crate::goals::{Goals, RunStatus};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status { Queued, Doing, Done, Failed, Cancelled }
impl Status {
    pub fn active(self) -> bool { matches!(self, Status::Queued | Status::Doing) }
    pub fn word(self) -> &'static str { match self { Status::Queued => "queued", Status::Doing => "doing", Status::Done => "done", Status::Failed => "failed", Status::Cancelled => "cancelled" } }
}

/// WHO WILL DO IT, decided when the item was queued. `role` is the router's verdict JSON (`roles::route_role`); `pair` the merge it names (planner x worker) when the pane lets
/// the router choose; `label` is the sentence the panel shows. `note` says when the binding was re-decided at the item's turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Binding { pub role: Value, pub pair: Value, pub label: String, pub why: String, pub mode: String, pub at: i64, #[serde(default)] pub note: String }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: i64, pub pane_id: i64,
    /// what the operator typed
    pub ask: String,
    /// the consolidated goal the run is planned from (whitespace-normalised; the refined ask when the front layer refined it)
    pub goal: String,
    pub status: Status,
    /// the plan request as the pane sent it (cwd, engine, n ...), replayed when the item's turn comes
    pub request: Value,
    pub binding: Binding,
    pub run_id: Option<i64>,
    pub cp_before: Option<Checkpoint>, pub cp_after: Option<Checkpoint>,
    pub created: i64, pub started: Option<i64>, pub ended: Option<i64>,
    pub note: String,
    /// files the goal names, and what it asks of them - deterministic, from the words (any language pack)
    pub paths: Vec<String>, pub creates: bool, pub removes: bool, pub reverts: bool,
}

/// the goal in one line of single spaces
pub fn consolidate(text: &str) -> String { text.split_whitespace().collect::<Vec<_>>().join(" ") }

/// How long a doing item may wait for its plan call to hand back a run: the planner's limit (10 min) plus the screen and slack. Past it the item is failed. Pure - tested.
pub const PLAN_WAIT_MS: i64 = 15 * 60_000;
pub fn plan_never_answered(started: Option<i64>, now: i64) -> bool { started.map(|s| now - s > PLAN_WAIT_MS).unwrap_or(false) }

pub struct Queues { items: Mutex<Vec<Item>>, seq: Mutex<i64>, path: Mutex<Option<std::path::PathBuf>> }
static GLOBAL: OnceLock<Arc<Queues>> = OnceLock::new();
/// the one queue store of this process (the goal runs read it for the state block; the endpoints write it)
pub fn global() -> Arc<Queues> { GLOBAL.get_or_init(|| Arc::new(Queues { items: Mutex::new(vec![]), seq: Mutex::new(0), path: Mutex::new(None) })).clone() }

/// The panel's whole story about one pane, as JSON: what the checkpoint history carries.
pub fn state_for(pane_id: i64) -> Value { global().state_json(pane_id) }
/// The text every model sees at the top of its prompt, empty when this pane has nothing queued or recorded. See `Queues::state_block`.
pub fn block_for(pane_id: i64, cp: Option<&Checkpoint>) -> String { global().state_block(pane_id, cp) }

pub type StartFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
/// starts the plan for one item: takes the plan request body, returns the run JSON (`{"id": ..}`) or the router's refusal
pub type StartFn = Arc<dyn Fn(Value) -> StartFuture + Send + Sync>;
pub type BindFuture = Pin<Box<dyn Future<Output = Binding> + Send>>;
/// decides the binding for (pane, goal): the router's verdict now
pub type BindFn = Arc<dyn Fn(i64, String) -> BindFuture + Send + Sync>;

pub enum Added { New(i64), Duplicate(i64) }

impl Queues {
    fn with<R>(&self, f: impl FnOnce(&mut Vec<Item>) -> R) -> R { f(&mut self.items.lock().unwrap_or_else(|p| p.into_inner())) }
    pub fn with_history(self: &Arc<Self>, path: std::path::PathBuf) -> usize {
        let loaded: Vec<Item> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let n = loaded.len();
        { let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner()); *s = (*s).max(loaded.iter().map(|i| i.id).max().unwrap_or(0)); }
        self.with(|items| *items = loaded);
        // NOTHING CAN BE IN FLIGHT AFTER A RESTART (operator 2026-10-09: the designer ask's item stayed "doing" with no run for ever and held every message behind it): an item that was starting when the
        // serve stopped never got its run id, and the pump waits for the plan call of an item like that. It is settled here.
        let now = crate::cli::now_ms();
        self.with(|items| for i in items.iter_mut() { if i.status == Status::Doing && i.run_id.is_none() { i.status = Status::Failed; i.ended = Some(now); i.note = "the serve restarted before the plan answered - ask again".into(); } });
        *self.path.lock().unwrap_or_else(|p| p.into_inner()) = Some(path);
        n
    }
    fn save(&self) {
        let Some(p) = self.path.lock().unwrap_or_else(|p| p.into_inner()).clone() else { return };
        let snapshot: Vec<Item> = self.with(|items| {
            // history is bounded per pane: every active item, and the newest 60 settled ones
            let mut keep: Vec<Item> = vec![]; let mut per: std::collections::HashMap<i64, usize> = Default::default();
            for i in items.iter().rev() { if i.status.active() { keep.push(i.clone()); } else { let c = per.entry(i.pane_id).or_default(); if *c < 60 { *c += 1; keep.push(i.clone()); } } }
            keep.reverse(); keep
        });
        let _ = p.parent().map(std::fs::create_dir_all);
        let tmp = p.with_extension("json.tmp");
        if serde_json::to_string(&snapshot).ok().and_then(|t| std::fs::write(&tmp, t).ok()).is_some() { let _ = std::fs::rename(&tmp, &p); }
    }
    fn next_id(&self) -> i64 { let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner()); *s += 1; *s }

    /// ADD AN ITEM. Consolidates the ask (one line, the paths it names, what it asks of them), refuses a duplicate of something already waiting or running in this pane
    /// (returns that item instead), and appends. Pure over the store - tested.
    pub fn add(&self, pane_id: i64, ask: &str, request: Value, binding: Binding, now: i64) -> Added {
        let goal = consolidate(ask);
        let norm = crate::lexicon::norm(&goal);
        if let Some(dup) = self.with(|items| items.iter().find(|i| i.pane_id == pane_id && i.status.active() && crate::lexicon::norm(&i.goal) == norm).map(|i| i.id)) { return Added::Duplicate(dup) }
        let id = self.next_id();
        let item = Item {
            id, pane_id, ask: ask.to_string(), paths: crate::screen::paths_in(&goal), creates: crate::screen::asks_to_create(&goal), removes: crate::lexicon::asks_to_remove(&goal), reverts: crate::lexicon::asks_to_revert(&goal),
            goal, status: Status::Queued, request, binding, run_id: None, cp_before: None, cp_after: None, created: now, started: None, ended: None, note: String::new(),
        };
        self.with(|items| items.push(item)); self.save();
        Added::New(id)
    }
    pub fn get(&self, id: i64) -> Option<Item> { self.with(|items| items.iter().find(|i| i.id == id).cloned()) }
    /// this pane's items in queue order: the newest `history` settled ones, then every active one
    pub fn list(&self, pane_id: i64, history: usize) -> Vec<Item> {
        self.with(|items| {
            let mine: Vec<&Item> = items.iter().filter(|i| i.pane_id == pane_id).collect();
            let settled: Vec<&Item> = mine.iter().copied().filter(|i| !i.status.active()).collect();
            let skip = settled.len().saturating_sub(history);
            settled.into_iter().skip(skip).chain(mine.iter().copied().filter(|i| i.status == Status::Doing)).chain(mine.iter().copied().filter(|i| i.status == Status::Queued)).cloned().collect()
        })
    }
    pub fn panes_with_work(&self) -> Vec<i64> { self.with(|items| { let mut p: Vec<i64> = items.iter().filter(|i| i.status.active()).map(|i| i.pane_id).collect(); p.sort(); p.dedup(); p }) }
    pub fn doing(&self, pane_id: i64) -> Option<Item> { self.with(|items| items.iter().find(|i| i.pane_id == pane_id && i.status == Status::Doing).cloned()) }
    pub fn next_queued(&self, pane_id: i64) -> Option<Item> { self.with(|items| items.iter().find(|i| i.pane_id == pane_id && i.status == Status::Queued).cloned()) }
    /// Take the agent's turn for `id` in ONE step: true (and Doing) only if nothing in the pane is Doing and `id` is the first queued. Two asks arriving at once cannot both start.
    pub fn claim_if_idle(&self, pane_id: i64, id: i64, now: i64) -> bool {
        let won = self.with(|items| {
            if items.iter().any(|i| i.pane_id == pane_id && i.status == Status::Doing) { return false }
            if items.iter().find(|i| i.pane_id == pane_id && i.status == Status::Queued).map(|i| i.id) != Some(id) { return false }
            let i = items.iter_mut().find(|i| i.id == id).unwrap(); i.status = Status::Doing; i.started = Some(now); true
        });
        if won { self.save(); } won
    }
    pub fn update(&self, id: i64, f: impl FnOnce(&mut Item)) -> Option<Item> { let r = self.with(|items| items.iter_mut().find(|i| i.id == id).map(|i| { f(i); i.clone() })); self.save(); r }

    /// CANCEL. A queued item is cancelled here (`None` back: nothing else to stop); a running one returns its run id so the caller can stop the run; a settled one cannot be.
    pub fn cancel(&self, id: i64, now: i64) -> Result<Option<i64>, String> {
        let r = self.with(|items| {
            let i = items.iter_mut().find(|i| i.id == id).ok_or("no such item")?;
            match i.status {
                Status::Queued => { i.status = Status::Cancelled; i.ended = Some(now); i.note = "cancelled by the operator before it started".into(); Ok(None) }
                // no run yet = the plan call is still in flight (or lost): there is nothing to stop, and waiting for a run id that may never come is what held the agent for ever
                Status::Doing if i.run_id.is_none() => { i.status = Status::Cancelled; i.ended = Some(now); i.note = "cancelled before the plan answered".into(); Ok(None) }
                Status::Doing => { i.note = "cancelling".into(); Ok(i.run_id) }
                _ => Err("it is already settled".to_string()),
            }
        });
        self.save(); r
    }
    /// MOVE a queued item to `index` among the pane's QUEUED items (0 = next). Only queued items move: the running one and the settled ones are history. Returns the new index.
    pub fn reorder(&self, id: i64, index: usize) -> Result<usize, String> {
        let r = self.with(|items| {
            let pane = items.iter().find(|i| i.id == id).ok_or("no such item")?.pane_id;
            if items.iter().find(|i| i.id == id).map(|i| i.status) != Some(Status::Queued) { return Err("only a queued item can be moved".to_string()) }
            let pos = items.iter().position(|i| i.id == id).unwrap();
            let item = items.remove(pos);
            let queued_pos: Vec<usize> = items.iter().enumerate().filter(|(_, i)| i.pane_id == pane && i.status == Status::Queued).map(|(k, _)| k).collect();
            let index = index.min(queued_pos.len());
            let at = queued_pos.get(index).copied().unwrap_or_else(|| queued_pos.last().map(|l| l + 1).unwrap_or(items.len()));
            items.insert(at, item);
            Ok(index)
        });
        self.save(); r
    }
    /// RE-PROMPT a queued item: new words, re-consolidated, its binding cleared for the caller to re-decide.
    pub fn reprompt(&self, id: i64, text: &str) -> Result<Item, String> {
        let goal = consolidate(text); if goal.is_empty() { return Err("the new prompt is empty".into()) }
        let r = self.with(|items| {
            let i = items.iter_mut().find(|i| i.id == id).ok_or("no such item")?;
            if i.status != Status::Queued { return Err("only a queued item can be re-prompted".to_string()) }
            i.ask = text.to_string(); i.paths = crate::screen::paths_in(&goal); i.creates = crate::screen::asks_to_create(&goal); i.removes = crate::lexicon::asks_to_remove(&goal); i.reverts = crate::lexicon::asks_to_revert(&goal);
            i.goal = goal; i.note = "re-prompted".into(); Ok(i.clone())
        });
        self.save(); r
    }

    /// The pane's queue as JSON for a checkpoint's message and the panel: done (newest 6), doing, queued (in order).
    pub fn state_json(&self, pane_id: i64) -> Value {
        let items = self.list(pane_id, 6);
        let row = |i: &Item| json!({ "id": i.id, "goal": i.goal.chars().take(160).collect::<String>(), "status": i.status.word(), "run": i.run_id, "role": i.binding.label, "cp": i.cp_after.as_ref().map(|c| c.n) });
        json!({
            "pane": pane_id,
            "done": items.iter().filter(|i| !i.status.active()).map(row).collect::<Vec<_>>(),
            "doing": items.iter().filter(|i| i.status == Status::Doing).map(row).collect::<Vec<_>>(),
            "queued": items.iter().filter(|i| i.status == Status::Queued).map(row).collect::<Vec<_>>(),
        })
    }
    /// THE BLOCK EVERY MODEL READS FIRST. Facts only, in the orchestrator's voice. `cp` is the checkpoint the reader's own work is measured against. Empty when there is nothing to say.
    pub fn state_block(&self, pane_id: i64, cp: Option<&Checkpoint>) -> String {
        let items = self.list(pane_id, 6);
        if items.is_empty() && cp.is_none() { return String::new() }
        let line = |i: &Item| format!("  - #{} “{}”{}", i.id, i.goal.chars().take(140).collect::<String>(), match i.status { Status::Done => " ✓", Status::Failed => " ✗ failed", Status::Cancelled => " (cancelled)", _ => "" });
        let mut out = String::from("[BOX STATE - kept by the orchestrator in code; authoritative, and the results below are measured against it whether or not you read it.\n");
        if let Some(c) = cp { out.push_str(&format!("The box as it stood before this work began is recorded{}. You are judged on what you change SINCE then; nothing before it is yours.\n", if c.n > 0 { format!(" (history is at cp:pane-{pane_id}/{})", c.n) } else { String::new() })); }
        let done: Vec<&Item> = items.iter().filter(|i| !i.status.active()).collect();
        if !done.is_empty() { out.push_str("DONE (already in the box - it stays):\n"); for i in done { out.push_str(&line(i)); out.push('\n'); } }
        let doing: Vec<&Item> = items.iter().filter(|i| i.status == Status::Doing).collect();
        if !doing.is_empty() { out.push_str("DOING (this is the work in progress):\n"); for i in doing { out.push_str(&line(i)); out.push('\n'); } }
        let queued: Vec<&Item> = items.iter().filter(|i| i.status == Status::Queued).collect();
        if !queued.is_empty() { out.push_str("QUEUED (in this order, not yours to do now):\n"); for i in queued { out.push_str(&line(i)); out.push('\n'); } }
        out.push_str("RULES: do not revert, undo, reset or delete anything that is not part of your own task. A result that throws away earlier work, changes nothing, or only deletes lines for a task that removes nothing is rejected by code before any reviewer reads it, and the box is put back.]\n");
        out
    }

    /// FINISH a doing item from its run's terminal status.
    fn settle(&self, id: i64, status: Status, note: String, cp: Option<Checkpoint>, now: i64) {
        self.update(id, |i| { i.status = status; i.ended = Some(now); if !note.is_empty() { i.note = note; } i.cp_after = cp; });
    }

    /// ONE STEP of the queue for every pane with work: settle the running item when its run is over, then start the next when nothing runs. Starting is spawned (a plan takes tens
    /// of seconds and must not hold the other panes). The binding is RE-DECIDED at the item's turn: if the router now says something else, the item says so and runs the fresh one.
    pub async fn tick(self: &Arc<Self>, goals: &Arc<Goals>, start: &StartFn, bind: &BindFn, now: i64) {
        for pane in self.panes_with_work() {
            if let Some(d) = self.doing(pane) {
                match d.run_id {
                    // the plan call is still in flight (the spawned start has not written the run id yet)
                    None => {
                        if plan_never_answered(d.started, now) { self.settle(d.id, Status::Failed, "the plan never answered - ask again".into(), None, now); }
                        continue
                    }
                    Some(rid) => match goals.get(rid) {
                        None => self.settle(d.id, Status::Failed, "the run is gone (the serve restarted?)".into(), None, now),
                        // QA is still judging (or has sent the run back to be fixed): it is not over
                        Some(r) if r.qa_pending => { let _ = r; }
                        Some(r) => {
                            let (st, note) = match r.status {
                                RunStatus::Done => (Some(Status::Done), r.report.as_deref().unwrap_or("").lines().map(|l| l.trim().trim_start_matches(['#', '-', '*', ' ']).trim()).find(|l| !l.is_empty() && !l.to_lowercase().starts_with("report")).unwrap_or("").chars().take(200).collect::<String>()),
                                RunStatus::Inline => (Some(Status::Done), "answered inline".to_string()),
                                RunStatus::Failed => (Some(Status::Failed), r.error.clone().unwrap_or_else(|| "the run failed".into())),
                                RunStatus::Cancelled => (Some(Status::Cancelled), "the run was cancelled".to_string()),
                                _ => (None, String::new()),
                            };
                            if let Some(st) = st {
                                let cwd = d.request.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
                                self.update(d.id, |i| i.status = st); // so the checkpoint's state block already shows it settled
                                let cp = if cwd.is_empty() { None } else { crate::checkpoint::take(&cwd, &format!("pane-{pane}"), &format!("after #{} - {}", d.id, st.word()), &self.state_json(pane), now) };
                                self.settle(d.id, st, note, cp, now);
                            }
                        }
                    },
                }
                continue; // one item at a time per agent: never start the next while this one holds the box
            }
            let Some(next) = self.next_queued(pane) else { continue };
            // claim it now, so the next tick cannot start it twice
            let cwd = next.request.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
            self.update(next.id, |i| { i.status = Status::Doing; i.started = Some(now); });
            let me = self.clone(); let (start, bind) = (start.clone(), bind.clone());
            let goals = goals.clone();
            tokio::spawn(async move {
                // an item cancelled while its plan was in flight must not leave the run it eventually got running on its own
                if let Ok(run) = me.start_item(next.id, &start, &bind, cwd, now).await {
                    if me.get(next.id).map(|i| i.status == Status::Cancelled).unwrap_or(false) {
                        if let Some(rid) = run.get("run").and_then(|r| r.get("id")).or_else(|| run.get("id")).and_then(Value::as_i64) { goals.cancel(rid); }
                    }
                }
            });
        }
    }

    /// Start one claimed (Doing) item: re-decide its binding, checkpoint the box, hand the request to the router. Also the path for an item added to an idle pane.
    pub async fn start_item(self: &Arc<Self>, id: i64, start: &StartFn, bind: &BindFn, cwd: String, now: i64) -> Result<Value, String> {
        let Some(item) = self.get(id) else { return Err("no such item".into()) };
        // 1 · THE BINDING, RE-DECIDED NOW
        let fresh = bind(item.pane_id, item.goal.clone()).await;
        let mut binding = item.binding.clone();
        if fresh.label != item.binding.label { binding = Binding { note: format!("re-bound at its turn: {} -> {}", if item.binding.label.is_empty() { "unrouted" } else { &item.binding.label }, if fresh.label.is_empty() { "unrouted" } else { &fresh.label }), ..fresh }; }
        // 2 · THE CHECKPOINT: the box as it is before this item touches it, with this item shown as doing
        self.update(id, |i| i.binding = binding.clone());
        let cp = if cwd.is_empty() { None } else { crate::checkpoint::take_if_changed(&cwd, &format!("pane-{}", item.pane_id), &format!("before #{} - edits made outside the queue", id), &self.state_json(item.pane_id), now) };
        self.update(id, |i| i.cp_before = cp.clone());
        // 3 · THE REQUEST: the pane's own body, with the goal as consolidated and the merge the queue chose (an explicit pair from the pane stays)
        let mut body = item.request.clone();
        body["goal"] = json!(item.goal); body["pane_id"] = json!(item.pane_id);
        if body.get("pair").map(|p| p.is_null()).unwrap_or(true) && !binding.pair.is_null() { body["pair"] = binding.pair.clone(); }
        match start(body).await {
            Ok(run) => {
                let rid = run.get("run").and_then(|r| r.get("id")).or_else(|| run.get("id")).and_then(Value::as_i64);
                self.update(id, |i| i.run_id = rid);
                Ok(run)
            }
            Err(why) => { self.settle(id, Status::Failed, why.clone(), None, now); Err(why) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn q() -> Queues { Queues { items: Mutex::new(vec![]), seq: Mutex::new(0), path: Mutex::new(None) } }
    fn b(label: &str) -> Binding { Binding { label: label.into(), ..Default::default() } }
    fn ids(q: &Queues, pane: i64) -> Vec<i64> { q.list(pane, 50).iter().map(|i| i.id).collect() }
    #[test]
    fn an_ask_is_consolidated_deduplicated_and_kept_per_agent() {
        let q = q();
        let Added::New(a) = q.add(1, "  add   filters\n by tag ", json!({}), b("Frontender"), 1) else { panic!() };
        assert_eq!(q.get(a).unwrap().goal, "add filters by tag");
        // the same words again, in this pane, while it waits: the existing item, not a second one
        assert!(matches!(q.add(1, "Add filters by tag!", json!({}), b("x"), 2), Added::Duplicate(d) if d == a));
        // another agent has its own queue
        assert!(matches!(q.add(2, "add filters by tag", json!({}), b("x"), 3), Added::New(_)));
        // what the words say is read in code, in any language
        let Added::New(r) = q.add(1, "remova a busca de src/App.tsx", json!({}), b("x"), 4) else { panic!() };
        let it = q.get(r).unwrap(); assert!(it.removes && !it.creates && !it.reverts && it.paths == vec!["src/App.tsx"], "{it:?}");
        let Added::New(u) = q.add(1, "undo the last change", json!({}), b("x"), 5) else { panic!() };
        assert!(q.get(u).unwrap().reverts);
    }
    #[test]
    fn queued_items_move_cancel_and_reprompt_and_nothing_else_does() {
        let q = q();
        let mut id = vec![]; for (k, g) in ["one", "two", "three", "four"].iter().enumerate() { let Added::New(i) = q.add(1, g, json!({}), b("r"), k as i64) else { panic!() }; id.push(i); }
        assert_eq!(ids(&q, 1), id);
        // drag the last to the front, and the first to the end
        assert_eq!(q.reorder(id[3], 0), Ok(0)); assert_eq!(ids(&q, 1), vec![id[3], id[0], id[1], id[2]]);
        assert_eq!(q.reorder(id[3], 99), Ok(3)); assert_eq!(ids(&q, 1), vec![id[0], id[1], id[2], id[3]]);
        // a running item is history: it neither moves nor is re-prompted, and cancelling it hands back its run
        q.update(id[0], |i| { i.status = Status::Doing; i.run_id = Some(77); });
        assert!(q.reorder(id[0], 2).is_err() && q.reprompt(id[0], "x").is_err());
        assert_eq!(q.cancel(id[0], 9), Ok(Some(77)));
        // queued: cancel is immediate, re-prompt re-reads the words
        assert_eq!(q.cancel(id[1], 9), Ok(None)); assert_eq!(q.get(id[1]).unwrap().status, Status::Cancelled);
        let r = q.reprompt(id[2], "remove the header").unwrap(); assert!(r.removes && r.goal == "remove the header");
        assert!(q.reprompt(id[2], "   ").is_err());
        // a settled one cannot be cancelled
        assert!(q.cancel(id[1], 10).is_err());
        // the order the panel sees: settled (history) first, then doing, then queued
        let order: Vec<Status> = q.list(1, 50).iter().map(|i| i.status).collect();
        assert_eq!(order, vec![Status::Cancelled, Status::Doing, Status::Queued, Status::Queued]);
    }
    #[test]
    fn the_state_every_model_reads_says_done_doing_and_queued() {
        let q = q();
        let Added::New(a) = q.add(1, "add a dark theme button", json!({}), b("r"), 1) else { panic!() };
        let Added::New(c) = q.add(1, "remove the search", json!({}), b("r"), 2) else { panic!() };
        let Added::New(d) = q.add(1, "add a sort dropdown", json!({}), b("r"), 3) else { panic!() };
        q.update(a, |i| { i.status = Status::Done; i.cp_after = Some(Checkpoint { rev: "abcdef123456".into(), tree: "t".into(), n: 4, label: "after".into(), t: 5 }); });
        q.update(c, |i| { i.status = Status::Doing; i.run_id = Some(259); });
        let js = q.state_json(1);
        assert_eq!(js["done"][0]["goal"], "add a dark theme button"); assert_eq!(js["done"][0]["cp"], 4);
        assert_eq!(js["doing"][0]["run"], 259); assert_eq!(js["queued"][0]["id"], d);
        let text = q.state_block(1, Some(&Checkpoint { rev: "0123456789ab".into(), tree: "t".into(), n: 5, label: "before".into(), t: 6 }));
        let (done, doing, queued) = (text.find("DONE").unwrap(), text.find("DOING").unwrap(), text.find("QUEUED").unwrap());
        assert!(done < doing && doing < queued, "{text}");
        assert!(text.contains("recorded (history is at cp:pane-1/5)") && text.contains("“remove the search”") && text.contains("do not revert"), "{text}");
        assert!(q.state_block(9, None).is_empty(), "a pane with nothing says nothing");
    }
    #[test]
    fn the_queue_survives_a_restart() {
        let p = std::env::temp_dir().join(format!("antiloki-queue-{}.json", std::process::id())); let _ = std::fs::remove_file(&p);
        let q1 = Arc::new(q()); q1.with_history(p.clone());
        let Added::New(a) = q1.add(3, "add filters", json!({ "cwd": "/x" }), b("r"), 1) else { panic!() };
        let q2 = Arc::new(q()); assert_eq!(q2.with_history(p.clone()), 1);
        assert_eq!(q2.get(a).unwrap().request["cwd"], "/x");
        assert!(matches!(q2.add(3, "add tests", json!({}), b("r"), 2), Added::New(n) if n > a), "ids keep counting");
        let _ = std::fs::remove_file(&p);
    }
    /// THE LOOP: one item at a time per agent, in order; the binding is re-decided at the item's turn and says so when it moved; another agent is not held up.
    #[tokio::test]
    async fn each_agent_runs_its_queue_in_order_one_at_a_time_and_rebinds_at_its_turn() {
        let (run_fn, _, _) = crate::goals::scripted_prompts_for_queue();
        let goals = crate::goals::Goals::new(run_fn, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 5);
        let q = Arc::new(q());
        let Added::New(a1) = q.add(1, "add filters", json!({ "cwd": "" }), b("Frontender · old"), 1) else { panic!() };
        let Added::New(a2) = q.add(1, "add a footer", json!({ "cwd": "" }), b("Frontender · same"), 2) else { panic!() };
        let Added::New(o1) = q.add(2, "fix login", json!({ "cwd": "" }), b("Backender"), 3) else { panic!() };
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
        // every "plan" the queue asks for becomes an inline run that is already over
        let start: StartFn = { let (g, seen) = (goals.clone(), seen.clone()); Arc::new(move |body: Value| { let (g, seen) = (g.clone(), seen.clone()); Box::pin(async move {
            seen.lock().unwrap().push(body.clone());
            let id = g.note_min_turn("p", crate::cli::Engine::ClaudeCode, ".", body["goal"].as_str().unwrap_or(""), crate::cli::Tier::Low, 1, 1, 1).unwrap();
            Ok(json!({ "ok": true, "run": { "id": id } }))
        }) }) };
        // the router now says something else for "add filters", and the same for the footer
        let bind: BindFn = Arc::new(|_pane, goal: String| Box::pin(async move { Binding { label: if goal.contains("filters") { "Frontender · new".into() } else if goal.contains("footer") { "Frontender · same".into() } else { "Backender".into() }, pair: json!({ "name": "p" }), ..Default::default() } }));
        for step in 0..12 { q.tick(&goals, &start, &bind, 100 + step).await; tokio::time::sleep(std::time::Duration::from_millis(30)).await; if [a1, a2, o1].iter().all(|i| q.get(*i).unwrap().status == Status::Done) { break } }
        assert!([a1, a2, o1].iter().all(|i| q.get(*i).unwrap().status == Status::Done), "{:?}", [a1, a2, o1].map(|i| q.get(i).unwrap().status));
        let order: Vec<String> = seen.lock().unwrap().iter().map(|b| b["goal"].as_str().unwrap().to_string()).collect();
        let pos = |g: &str| order.iter().position(|x| x == g).unwrap();
        assert!(pos("add filters") < pos("add a footer"), "in the order asked: {order:?}");
        // the moved binding is said on the item; the unchanged one is not
        assert!(q.get(a1).unwrap().binding.note.contains("re-bound at its turn: Frontender · old -> Frontender · new"), "{:?}", q.get(a1).unwrap().binding);
        assert!(q.get(a2).unwrap().binding.note.is_empty());
        // the merge the queue chose rides in the plan request
        assert_eq!(seen.lock().unwrap()[0]["pair"]["name"], "p");
        assert_eq!(seen.lock().unwrap()[0]["pane_id"], 1);
        // a refusal from the router settles the item as failed with the reason, and the next item still gets its turn
        let Added::New(f1) = q.add(3, "will be refused", json!({ "cwd": "" }), b("x"), 9) else { panic!() };
        let Added::New(f2) = q.add(3, "will run", json!({ "cwd": "" }), b("x"), 9) else { panic!() };
        let refuse: StartFn = { let g = goals.clone(); Arc::new(move |body: Value| { let g = g.clone(); Box::pin(async move {
            if body["goal"] == "will be refused" { return Err("delegation is off for this pane".to_string()) }
            let id = g.note_min_turn("p", crate::cli::Engine::ClaudeCode, ".", "x", crate::cli::Tier::Low, 1, 1, 1).unwrap(); Ok(json!({ "run": { "id": id } }))
        }) }) };
        for step in 0..12 { q.tick(&goals, &refuse, &bind, 200 + step).await; tokio::time::sleep(std::time::Duration::from_millis(30)).await; if q.get(f2).unwrap().status == Status::Done { break } }
        assert_eq!((q.get(f1).unwrap().status, q.get(f1).unwrap().note.as_str()), (Status::Failed, "delegation is off for this pane"));
        assert_eq!(q.get(f2).unwrap().status, Status::Done);
    }

    // OPERATOR 2026-10-09: the designer ask's item stayed "doing" with no run (its plan call hung / the serve restarted), and every message after it queued behind it for ever.
    fn started_without_a_run(q: &Queues, now: i64) -> i64 {
        let Added::New(id) = q.add(1, "shift the ui", json!({}), b("Designer"), now) else { panic!() };
        q.update(id, |i| { i.status = Status::Doing; i.started = Some(now); });
        id
    }
    #[test]
    fn cancelling_an_item_whose_plan_has_not_answered_settles_it_and_frees_the_agent() {
        let q = q(); let id = started_without_a_run(&q, 10);
        let Added::New(next) = q.add(1, "make it gamer like", json!({}), b("Designer"), 11) else { panic!() };
        assert_eq!(q.cancel(id, 20), Ok(None), "nothing to stop: there is no run");
        let it = q.get(id).unwrap();
        assert_eq!((it.status, it.ended), (Status::Cancelled, Some(20)));
        assert!(q.doing(1).is_none(), "the agent is free");
        assert_eq!(q.next_queued(1).map(|i| i.id), Some(next), "and the message behind it is next");
    }
    #[test]
    fn a_plan_that_never_answered_is_given_up_on_after_fifteen_minutes() {
        assert!(!plan_never_answered(Some(0), 14 * 60_000));
        assert!(plan_never_answered(Some(0), 16 * 60_000));
        assert!(!plan_never_answered(None, 99 * 60_000), "an item with no start time is left to the other rules");
    }
    #[test]
    fn an_item_that_was_starting_when_the_serve_stopped_is_failed_at_boot() {
        let dir = std::env::temp_dir().join(format!("antiloki-queue-{}-{}", std::process::id(), crate::cli::now_ms()));
        let path = dir.join("queues.json");
        let a = Arc::new(q()); a.with_history(path.clone());
        let id = started_without_a_run(&a, 10);
        let Added::New(next) = a.add(1, "behind it", json!({}), b("x"), 11) else { panic!() };
        let again = Arc::new(Queues { items: Mutex::new(vec![]), seq: Mutex::new(0), path: Mutex::new(None) });
        assert_eq!(again.with_history(path), 2);
        let it = again.get(id).unwrap();
        assert_eq!(it.status, Status::Failed);
        assert!(it.note.contains("restarted"), "{}", it.note);
        assert!(again.doing(1).is_none() && again.next_queued(1).map(|i| i.id) == Some(next));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
