//! Port of EventsService + the @Sse('events') endpoint.
//!
//! An in-memory hub that multicasts "substrate changed" pings to every connected SSE client. The
//! FE refetches on these pings instead of polling, so an idle board makes zero requests. The
//! stream carries no data — only coarse notices saying WHEN to refetch, never WHAT changed.
//!
//! Two behaviours are contractual, not incidental:
//!   * **200ms coalescing.** A burst of dispatches (the tick loop chaining stages) collapses into
//!     ONE change whose topics are the union of the burst. Without it, N emits fan out into N×M
//!     FE refetches.
//!   * **20s heartbeat.** Lets the FE distinguish "stream healthy, nothing happening" from
//!     "stream dead". A silent stream is indistinguishable from a hung one.

use axum::response::sse::{Event, Sse};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_stream::{Stream, StreamExt};

const COALESCE_MS: u64 = 200;
const HEARTBEAT_MS: u64 = 20_000;

#[derive(Default)]
struct Pending {
    any: bool,
    topics: BTreeSet<String>,
    /// keyed "kind:id" so repeated entity mentions collapse, with `delete` sticky
    entities: BTreeMap<String, (String, Option<String>, Option<String>)>,
    action: Option<String>,
    outcome: Option<String>,
}

pub struct EventsHub {
    tx: broadcast::Sender<Value>,
    pending: Mutex<Pending>,
    flush_armed: Mutex<bool>,
}

impl EventsHub {
    pub fn new() -> Arc<Self> {
        let (tx, _) = broadcast::channel(256);
        Arc::new(Self { tx, pending: Mutex::new(Pending::default()), flush_armed: Mutex::new(false) })
    }

    /// how many SSE clients are attached right now
    pub fn listeners(&self) -> usize { self.tx.receiver_count() }
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }

    /// Announce a change. Cheap with no subscribers, exactly like `Subject.next` with no observers.
    pub fn emit(
        self: &Arc<Self>,
        action: Option<String>,
        outcome: Option<String>,
        topics: Vec<String>,
        entities: Vec<(String, Option<String>, Option<String>)>,
    ) {
        {
            let mut p = self.pending.lock().unwrap();
            p.any = true;
            for t in topics {
                p.topics.insert(t);
            }
            for (kind, id, op) in entities {
                let key = format!("{kind}:{}", id.clone().unwrap_or_else(|| "*".into()));
                // delete is sticky; otherwise last op wins
                let op = match p.pending_op(&key) {
                    Some(prev) if prev == "delete" => Some("delete".to_string()),
                    _ => op,
                };
                p.entities.insert(key, (kind, id, op));
            }
            if action.is_some() {
                p.action = action;
            }
            if outcome.is_some() {
                p.outcome = outcome;
            }
        }

        let mut armed = self.flush_armed.lock().unwrap();
        if *armed {
            return;
        }
        *armed = true;
        drop(armed);

        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(COALESCE_MS)).await;
            *me.flush_armed.lock().unwrap() = false;
            me.flush();
        });
    }

    fn flush(&self) {
        let mut p = self.pending.lock().unwrap();
        if !p.any {
            return;
        }
        let mut change = serde_json::Map::new();
        change.insert("t".into(), json!("change"));
        change.insert("ts".into(), json!(crate::events::iso_now()));
        if !p.topics.is_empty() {
            change.insert("topics".into(), json!(p.topics.iter().cloned().collect::<Vec<_>>()));
        }
        if !p.entities.is_empty() {
            let list: Vec<Value> = p
                .entities
                .values()
                .map(|(kind, id, op)| {
                    let mut e = serde_json::Map::new();
                    e.insert("kind".into(), json!(kind));
                    if let Some(i) = id {
                        e.insert("id".into(), json!(i));
                    }
                    if let Some(o) = op {
                        e.insert("op".into(), json!(o));
                    }
                    Value::Object(e)
                })
                .collect();
            change.insert("entities".into(), json!(list));
        }
        if let Some(a) = &p.action {
            change.insert("action".into(), json!(a));
        }
        if let Some(o) = &p.outcome {
            change.insert("outcome".into(), json!(o));
        }

        *p = Pending::default();
        drop(p);
        let _ = self.tx.send(Value::Object(change)); // Err only means "no subscribers"
    }
}

impl Pending {
    fn pending_op(&self, key: &str) -> Option<&str> {
        self.entities.get(key).and_then(|(_, _, op)| op.as_deref())
    }
}

/// `new Date().toISOString()` — the FE parses these, so the shape must match exactly:
/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, always UTC, always 3-digit milliseconds.
pub fn iso_now() -> String {
    antiloki_rs::clock::now_iso_nodb()
}

/// GET /api/v1/events — the one SSE route. Merges the change stream with a heartbeat, mirroring
/// `merge(changes, heartbeat)` in the Nest controller.
pub async fn sse_handler(
    hub: Arc<EventsHub>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = hub.subscribe();
    let changes = tokio_stream::wrappers::BroadcastStream::new(rx)
        .filter_map(|r| r.ok())
        .map(|v| Ok(Event::default().data(v.to_string())));

    let heartbeat = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
        Duration::from_millis(HEARTBEAT_MS),
    ))
    .map(|_| Ok(Event::default().data(json!({ "t": "ping", "ts": iso_now() }).to_string())));

    Sse::new(changes.merge(heartbeat))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use std::time::Duration;

    fn s(x: &str) -> String { x.to_string() }
    fn ent(kind: &str, id: Option<&str>, op: Option<&str>) -> (String, Option<String>, Option<String>) { (s(kind), id.map(s), op.map(s)) }
    async fn next_change(rx: &mut broadcast::Receiver<Value>) -> Value {
        tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.expect("a change within 3 s").expect("channel open")
    }

    /// RULE: a burst of dispatches is ONE change whose topics are the union of the burst (200 ms coalescing).
    #[tokio::test]
    async fn a_burst_of_emits_is_one_change_with_the_union_of_the_topics() {
        let hub = EventsHub::new();
        let mut rx = hub.subscribe();
        hub.emit(None, None, vec![s("driver"), s("card:b")], vec![]);
        hub.emit(None, None, vec![s("card:a"), s("driver")], vec![]);
        hub.emit(None, None, vec![], vec![]);
        let c = next_change(&mut rx).await;
        assert_eq!(c["t"], "change");
        assert_eq!(c["topics"], json!(["card:a", "card:b", "driver"]), "deduplicated and sorted");
        assert!(c.get("entities").is_none() && c.get("action").is_none() && c.get("outcome").is_none(), "absent keys stay absent: {c}");
        tokio::time::sleep(Duration::from_millis(COALESCE_MS + 150)).await;
        assert!(rx.try_recv().is_err(), "three emits, one message");
    }

    /// RULE: after a flush the hub re-arms - the next burst is a NEW change, carrying nothing of the last one.
    #[tokio::test]
    async fn the_next_burst_after_a_flush_is_a_fresh_change() {
        let hub = EventsHub::new();
        let mut rx = hub.subscribe();
        hub.emit(Some(s("first")), Some(s("fired")), vec![s("one")], vec![ent("card", Some("x"), Some("update"))]);
        let a = next_change(&mut rx).await;
        hub.emit(None, None, vec![s("two")], vec![]);
        let b = next_change(&mut rx).await;
        assert_eq!(a["topics"], json!(["one"]));
        assert_eq!((a["action"].as_str(), a["outcome"].as_str()), (Some("first"), Some("fired")));
        assert_eq!(b["topics"], json!(["two"]), "the second change is not the union with the first");
        assert!(b.get("entities").is_none() && b.get("action").is_none() && b.get("outcome").is_none(), "and inherits none of its detail: {b}");
    }

    /// RULE: entities collapse by kind+id, the last op wins, and a delete is sticky (a card deleted in a burst is never announced as updated).
    #[tokio::test]
    async fn entity_mentions_collapse_the_last_op_wins_and_delete_is_sticky() {
        let hub = EventsHub::new();
        let mut rx = hub.subscribe();
        hub.emit(None, None, vec![], vec![
            ent("card", Some("b"), Some("create")),
            ent("card", Some("a"), Some("update")),
            ent("card", Some("b"), Some("update")),   // last op wins over create
            ent("card", Some("c"), Some("delete")),
            ent("card", Some("c"), Some("update")),   // ...except after a delete
            ent("driver", None, None),
            ent("card", Some("a"), Some("update")),
        ]);
        hub.flush(); // deterministic: do not wait for the timer
        let c = rx.try_recv().expect("flushed");
        assert_eq!(c["entities"], json!([
            { "kind": "card", "id": "a", "op": "update" },
            { "kind": "card", "id": "b", "op": "update" },
            { "kind": "card", "id": "c", "op": "delete" },
            { "kind": "driver" },
        ]), "one row per kind:id, ordered by key; id and op omitted when the mention had none");
    }

    /// RULE: a delete stays a delete even when it arrives in a LATER emit of the same burst.
    #[tokio::test]
    async fn a_delete_survives_a_later_emit_of_the_same_burst() {
        let hub = EventsHub::new();
        let mut rx = hub.subscribe();
        hub.emit(None, None, vec![], vec![ent("card", Some("z"), Some("delete"))]);
        hub.emit(None, None, vec![], vec![ent("card", Some("z"), Some("create"))]);
        hub.flush();
        assert_eq!(rx.try_recv().unwrap()["entities"], json!([{ "kind": "card", "id": "z", "op": "delete" }]));
    }

    /// RULE: the burst's action/outcome are the last ones SAID; an emit that says nothing does not erase them.
    #[tokio::test]
    async fn the_last_action_and_outcome_named_survive_a_silent_emit() {
        let hub = EventsHub::new();
        let mut rx = hub.subscribe();
        hub.emit(Some(s("a1")), Some(s("fired")), vec![], vec![]);
        hub.emit(Some(s("a2")), None, vec![], vec![]);
        hub.emit(None, Some(s("failed")), vec![], vec![]);
        hub.emit(None, None, vec![], vec![]);
        hub.flush();
        let c = rx.try_recv().unwrap();
        assert_eq!((c["action"].as_str(), c["outcome"].as_str()), (Some("a2"), Some("failed")));
    }

    /// RULE: nothing pending means nothing sent (a stray flush is not a change), and a flush empties the hub.
    #[tokio::test]
    async fn a_flush_with_nothing_pending_sends_nothing_and_a_flush_empties_the_hub() {
        let hub = EventsHub::new();
        let mut rx = hub.subscribe();
        hub.flush();
        assert!(rx.try_recv().is_err(), "no change without an emit");
        hub.emit(None, None, vec![s("t")], vec![]);
        hub.flush();
        assert!(rx.try_recv().is_ok());
        hub.flush();
        assert!(rx.try_recv().is_err(), "the second flush finds nothing");
        // an emit that carried nothing at all is still a change (the FE refetches on the ping)
        hub.emit(None, None, vec![], vec![]);
        hub.flush();
        let c = rx.try_recv().unwrap();
        assert_eq!(c["t"], "change");
        assert!(c.get("topics").is_none());
    }

    /// RULE: emitting with nobody listening is cheap and safe, and a late subscriber hears only what comes after it joined.
    #[tokio::test]
    async fn emitting_with_no_subscribers_is_harmless_and_late_subscribers_hear_only_later_changes() {
        let hub = EventsHub::new();
        assert_eq!(hub.listeners(), 0);
        hub.emit(None, None, vec![s("early")], vec![]);
        hub.flush();
        let mut rx = hub.subscribe();
        assert_eq!(hub.listeners(), 1);
        assert!(rx.try_recv().is_err(), "the early change is not replayed");
        hub.emit(None, None, vec![s("late")], vec![]);
        hub.flush();
        assert_eq!(rx.try_recv().unwrap()["topics"], json!(["late"]));
        let rx2 = hub.subscribe();
        assert_eq!(hub.listeners(), 2);
        drop(rx2);
        drop(rx);
        assert_eq!(hub.listeners(), 0, "listeners tracks live receivers");
    }

    /// RULE: every change is stamped `YYYY-MM-DDTHH:MM:SS.mmmZ` - the FE parses it.
    #[test]
    fn timestamps_are_utc_iso_with_milliseconds() {
        let t = iso_now();
        let b = t.as_bytes();
        assert_eq!(t.len(), 24, "{t}");
        for (i, want) in [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':'), (19, b'.'), (23, b'Z')] { assert_eq!(b[i], want, "{t} at {i}"); }
        assert!(t.bytes().enumerate().all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19 | 23) || c.is_ascii_digit()), "{t}");
    }

    async fn frame(body: &mut (impl tokio_stream::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin)) -> String {
        let f = tokio::time::timeout(Duration::from_secs(3), body.next()).await.expect("a frame within 3 s").expect("stream open").expect("no body error");
        String::from_utf8(f.to_vec()).unwrap()
    }

    /// RULE: the SSE stream opens with a heartbeat (so "healthy and quiet" is distinguishable from "dead"), then carries each change as one `data:` frame.
    #[tokio::test]
    async fn the_stream_opens_with_a_ping_then_carries_changes_and_detaches_when_dropped() {
        let hub = EventsHub::new();
        let res = sse_handler(hub.clone()).await.into_response();
        assert_eq!(res.headers().get("content-type").and_then(|v| v.to_str().ok()), Some("text/event-stream"));
        assert_eq!(hub.listeners(), 1, "attached the moment the handler ran");
        let mut body = res.into_body().into_data_stream();
        let first = frame(&mut body).await;
        assert!(first.starts_with("data: ") && first.ends_with("\n\n"), "{first:?}");
        let ping: Value = serde_json::from_str(first.trim_start_matches("data: ").trim()).unwrap();
        assert_eq!(ping["t"], "ping");
        assert_eq!(ping["ts"].as_str().unwrap().len(), 24);
        hub.emit(None, Some(s("fired")), vec![s("driver")], vec![]);
        let second = frame(&mut body).await;
        let change: Value = serde_json::from_str(second.trim_start_matches("data: ").trim()).unwrap();
        assert_eq!((change["t"].as_str(), change["outcome"].as_str(), change["topics"].clone()), (Some("change"), Some("fired"), json!(["driver"])));
        drop(body);
        assert_eq!(hub.listeners(), 0, "a closed connection frees its slot");
    }

    /// RULE: a client too slow to keep up loses old pings (they are only "refetch now" hints) but its stream lives on.
    #[tokio::test]
    async fn a_slow_client_that_lags_loses_old_pings_not_the_stream() {
        let hub = EventsHub::new();
        let res = sse_handler(hub.clone()).await.into_response();
        let mut body = res.into_body().into_data_stream();
        let _ping = frame(&mut body).await;
        for i in 0..400 { let _ = hub.tx.send(json!({ "t": "change", "n": i })); } // more than the channel holds (256)
        let got = frame(&mut body).await;
        let v: Value = serde_json::from_str(got.trim_start_matches("data: ").trim()).unwrap();
        assert_eq!(v["t"], "change", "the stream survived the lag and delivers a change, not an error");
        assert!(v["n"].as_i64().unwrap() >= 144, "the oldest were dropped: {v}");
    }
}
