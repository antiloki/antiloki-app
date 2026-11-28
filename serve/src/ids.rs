//! UNIQUE IDS FOR EVERYTHING THAT CAN BE POINTED AT (operator 2026-09-29: "add unique ids for warnings, everything should be able to tell apart").
//!
//! An id is `<prefix><time in hex>-<counter in hex>`: unique inside one process by the counter, across restarts by the time. The prefix says what it is, so a quoted id is
//! self-describing: `W` a warning (a warn/bad note, a rejection, a failed gate, a send-back), `e` a stored pane event, `g` a goal-run event. Queue items keep their own
//! numbers (`#12`), checkpoints are `cp:<pane>/<n>` (`checkpoint::name`), and QA send-backs `Q<run>.<round>`.
use std::sync::atomic::{AtomicU64, Ordering};
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// a fresh id with `prefix`. Pure but for the counter - tested.
pub fn next(prefix: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{:x}-{:03x}", crate::cli::now_ms(), n & 0xfff)
}
/// the goal-run event kinds that are warnings: something was rejected, retried, refused, put back or failed
pub fn goal_event_is_warning(kind: &str) -> bool {
    matches!(kind, "reject" | "retry" | "gate" | "fallback" | "error" | "fail" | "qa-gate-fail" | "qa-fail" | "qa-warn" | "qa-send-back" | "cancel" | "screen-warn" | "worker-error")
}
/// the prefix of a stored pane event: a note whose tone is warn or bad is a warning
pub fn pane_event_prefix(ev: &serde_json::Value) -> &'static str {
    let warn = ev.get("type").and_then(|t| t.as_str()) == Some("antiloki") && matches!(ev.get("tone").and_then(|t| t.as_str()), Some("warn") | Some("bad"));
    if warn { "W" } else { "e" }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_id_is_different_and_says_what_it_is() {
        let ids: Vec<String> = (0..2000).map(|_| next("W")).collect();
        let set: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(set.len(), ids.len(), "2000 ids, 2000 different ones");
        assert!(ids.iter().all(|i| i.starts_with('W') && i.contains('-')));
        assert_eq!(pane_event_prefix(&serde_json::json!({ "type": "antiloki", "subtype": "note", "tone": "warn" })), "W");
        assert_eq!(pane_event_prefix(&serde_json::json!({ "type": "antiloki", "subtype": "note", "tone": "dim" })), "e");
        assert_eq!(pane_event_prefix(&serde_json::json!({ "type": "assistant" })), "e");
        assert!(goal_event_is_warning("reject") && goal_event_is_warning("gate") && !goal_event_is_warning("pass") && !goal_event_is_warning("start"));
    }
}
