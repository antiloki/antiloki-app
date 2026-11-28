//! ANTICHAT'S AUTOMATIC CONTEXT (operator 2026-09-21: "each chat is a session, read-only, has access to all
//! context from all agents, automatically chooses and finds the context needed for the answer"). One call
//! before a chat turn: a DETERMINISTIC retriever (no AI, $0) picks what to attach, capped —
//!   · the agents' chats (every pane of the workspace, any thread) that speak of what the ask names;
//!   · the goal runs (plans, QA verdicts, what the router saved) when the ask is about work or money;
//!   · the codebase intel (antiIDE's smart context bundle) when the ask names files, symbols or code words.
//! Nothing matches → nothing is attached (a plain question stays cheap). The picks come back named
//! (`used`) so the chat can show them as chips; `pins` always ride, `drops` never do.
//! Pure scorer and composer — tested; the handler only gathers.
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use crate::dispatch::AppState;
use crate::goals::{Goals, GoalRun, RunStatus};

const STOP: &[&str] = &["the", "and", "for", "that", "this", "with", "from", "what", "does", "have", "has", "are", "was", "were", "you", "your", "our", "its", "it's", "about", "into", "than", "then", "them", "they", "there", "here", "how", "why", "when", "which", "who", "can", "could", "would", "should", "will", "just", "like", "also", "not", "but", "did", "one", "all", "any", "some", "please", "tell", "show", "give", "want", "need", "know", "think", "make", "let", "get", "got"];
/// The words of an ask worth matching: lowercase, ≥ 3 chars, no stop words; path-like tokens kept whole
/// (`src/cart.ts`) AND split (`cart`). Pure.
pub fn keywords(ask: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![]; let mut seen = HashSet::new();
    let mut push = |w: String| { if w.len() >= 3 && !STOP.contains(&w.as_str()) && seen.insert(w.clone()) { out.push(w); } };
    for tok in ask.split(|c: char| c.is_whitespace() || ",;:!?\"'()[]{}<>`".contains(c)) {
        let t = tok.trim_matches(|c: char| ".-_/#@".contains(c)).to_lowercase();
        if t.is_empty() { continue; }
        if t.contains('/') || t.contains('.') { push(t.clone()); for part in t.split(|c| c == '/' || c == '.' || c == '-' || c == '_') { push(part.to_string()); } }
        else { push(t); }
    }
    out
}
/// How many of the keywords a text mentions (each once). Pure.
pub fn score(text: &str, kws: &[String]) -> usize { let t = text.to_lowercase(); kws.iter().filter(|k| t.contains(k.as_str())).count() }
/// Does the ask speak of work or money — plans, QA, cost? Pure.
pub fn about_work(ask: &str) -> bool {
    let a = ask.to_lowercase();
    ["plan", "run", "task", "sub-agent", "subagent", "qa", "judge", "verdict", "cost", "spent", "spend", "saved", "money", "$", "usd", "token", "router", "heimdall", "agent", "doing", "progress", "status"].iter().any(|w| a.contains(w))
}
/// Does the ask speak of code — a path, a symbol, or code words? Pure.
pub fn about_code(ask: &str) -> bool {
    let a = ask.to_lowercase();
    a.split_whitespace().any(|w| w.contains('/') || w.matches('.').count() >= 1 && w.rsplit('.').next().map(|e| ["ts", "tsx", "js", "jsx", "rs", "py", "go", "md", "css", "json", "html", "java", "rb"].contains(&e)).unwrap_or(false))
        || ["file", "function", "class", "component", "import", "module", "test", "coverage", "law", "health", "review", "coupling", "structure", "cohesion", "defect", "smell", "cycle", "graph", "blast", "architecture", "folder", "src", "code", "bug", "refactor", "hook", "type"].iter().any(|w| a.split(|c: char| !c.is_alphanumeric()).any(|t| t == *w))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Pick { pub kind: String, pub label: String, pub r#ref: String, pub why: String, pub md: String, pub chars: usize }
#[derive(Clone)]
pub struct Candidate { pub kind: String, pub label: String, pub r#ref: String, pub text: String, pub base: usize }

/// Rank the candidates for an ask and compose the context: pins first (always), then by score (≥ `min`),
/// never a drop; the total capped at `cap` chars (a section that does not fit is trimmed from its end,
/// the rest skipped). Pure — tested.
pub fn compose(ask: &str, cands: Vec<Candidate>, pins: &[String], drops: &[String], cap: usize, min: usize) -> (String, Vec<Pick>) {
    let kws = keywords(ask);
    let mut ranked: Vec<(usize, bool, Candidate)> = cands.into_iter().filter(|c| !drops.contains(&c.r#ref)).map(|c| { let pinned = pins.contains(&c.r#ref); let s = score(&c.text, &kws) + c.base; (s, pinned, c) }).filter(|(s, pinned, _)| *pinned || *s >= min).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    let mut used = vec![]; let mut parts = vec![]; let mut left = cap;
    for (s, pinned, c) in ranked {
        if left < 400 { break; }
        let mut md = c.text.trim().to_string();
        if md.chars().count() > left { md = format!("{}…", md.chars().take(left - 1).collect::<String>()); }
        let n = md.chars().count(); left -= n;
        let why = if pinned { "pinned".to_string() } else { format!("{s} match{}", if s == 1 { "" } else { "es" }) };
        parts.push(format!("### {} — {}\n\n{}", c.label, why, md));
        used.push(Pick { kind: c.kind, label: c.label, r#ref: c.r#ref, why, md: String::new(), chars: n });
    }
    if parts.is_empty() { return (String::new(), vec![]); }
    let md = format!("## Context antiloki looked up for this ask (read it before answering)\n\nYou are antiChat: read-only. You never change code or run a box — when the answer calls for a change, say which agent should do it (or that a specialist could) and what to tell it.\n\n{}", parts.join("\n\n"));
    (md, used)
}
/// A goal run as a short card for the chat. Pure.
pub fn run_card(r: &GoalRun) -> String {
    let goal = r.goal.lines().next().unwrap_or_default().chars().take(140).collect::<String>();
    let status = match r.status { RunStatus::Inline => "inline".to_string(), _ => format!("{:?}", r.status).to_lowercase() };
    let tasks = if r.tasks.is_empty() { String::new() } else { format!(" · {} task{}", r.tasks.len(), if r.tasks.len() == 1 { "" } else { "s" }) };
    let qa = r.qa_review.as_ref().map(|q| format!(" · QA {} {}/100{}", if q.passed { "passed" } else { "FAILED" }, q.score, if q.summary.is_empty() { String::new() } else { format!(" — {}", q.summary.chars().take(160).collect::<String>()) })).unwrap_or_default();
    let money = r.savings.as_ref().map(|s| format!(" · ${:.3} (saved ${:.3})", s.actual_usd, s.saved_usd)).unwrap_or_default();
    let route = r.route.as_ref().map(|x| format!(" · route: tier {:?}{}", x.tier, if x.delegate { ", delegated" } else { "" })).unwrap_or_default();
    let box_ = r.cwd.rsplit('/').next().unwrap_or("");
    format!("- goal run #{} · {status}{tasks} · box {box_}{route}{money}{qa}\n  {goal}{}", r.id, r.report.as_ref().map(|x| format!("\n  report: {}", x.lines().next().unwrap_or_default().chars().take(200).collect::<String>())).unwrap_or_default())
}

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }

/// `POST /api/v1/chat/auto_context {ask, cap?, pins?, drops?}`
pub async fn auto_context(state: Arc<AppState>, goals: Arc<Goals>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let ask = s(b.get("ask")).trim().to_string();
    if ask.is_empty() { return (StatusCode::BAD_REQUEST, Json(json!({ "error": { "message": "ask required" } }))).into_response(); }
    let cap = b.get("cap").and_then(Value::as_u64).unwrap_or(8_000).clamp(1_000, 30_000) as usize;
    let strs = |k: &str| b.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>()).unwrap_or_default();
    let (pins, drops) = (strs("pins"), strs("drops"));
    let t0 = std::time::Instant::now();
    let (md, used) = compose_for(&state, &goals, &ask, cap, &pins, &drops, None).await;
    Json(json!({ "ok": true, "md": md, "used": used, "chars": md.chars().count(), "ms": t0.elapsed().as_millis() as u64 })).into_response()
}
/// The picks for an ask — shared by the route and the chat panes (chat_pane.rs). `exclude_pane`: the asking
/// chat's own log never rides (it is the conversation itself).
pub async fn compose_for(state: &Arc<AppState>, goals: &Arc<Goals>, ask: &str, cap: usize, pins: &[String], drops: &[String], exclude_pane: Option<i64>) -> (String, Vec<Pick>) {
    let ask = ask.to_string();
    let mut cands: Vec<Candidate> = vec![];
    // the agents' chats — every pane of the workspace's open sessions
    let conn = state.pick();
    // EVERY SEAT, not three of them: `chat_pane::ENGINES` is the one list (agents/live had its own 3-list and dropped the Gemini, Grok
    // and Cursor agents from the bar until the audit of 2026-09-28; here their chats were missing from antiChat's context the same way)
    let kinds = crate::chat_pane::ENGINES.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(", ");
    let panes = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, &format!("SELECT p.id, s.name AS session_name, p.settings_json, p.worktree_path FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE s.status != 'closed' AND COALESCE(p.status, '') != 'closed' AND p.worktree_path IS NOT NULL AND p.worktree_path != '' AND p.pane_kind IN ({kinds}) ORDER BY p.id DESC"), &[]).unwrap_or(json!([]))
    }).await.unwrap_or(json!([]));
    // One candidate per AGENT, not per box. The old dedup was by worktree because every agent appended to the same
    // `agent-events.jsonl`, so reading it once per pane produced the same transcript N times over. Now each pane has
    // its own log, so several agents sharing a worktree are several distinct chats and each gets its own card.
    for p in panes.as_array().cloned().unwrap_or_default() {
        let id = p.get("id").and_then(Value::as_i64).unwrap_or(0);
        if exclude_pane.is_some() && Some(id) == exclude_pane { continue; }
        let wt = s(p.get("worktree_path"));
        let text = std::fs::read_to_string(crate::agent::events_read_path(&wt, id)).unwrap_or_default();
        if text.is_empty() { continue; }
        let md = crate::agents_live::transcript_from_lines(text.lines(), 3_000);
        if md.trim().is_empty() { continue; }
        let settings = serde_json::from_str::<Value>(&s(p.get("settings_json"))).unwrap_or(json!({}));
        let name = settings.get("recipe").and_then(|r| r.get("name")).and_then(Value::as_str).map(String::from).unwrap_or_else(|| s(p.get("session_name")));
        cands.push(Candidate { kind: "agent".into(), label: format!("the chat of the agent {name} (box {})", wt.rsplit('/').next().unwrap_or("")), r#ref: format!("agent:{id}"), text: md, base: 0 });
    }
    // the goal runs: the latest few as one card when the ask is about work or money; any run the ask names
    let runs = goals.list(None);
    let work = about_work(&ask);
    let mut settled: Vec<&GoalRun> = runs.iter().filter(|r| r.status != RunStatus::Inline || r.qa_review.is_some() || r.savings.is_some()).collect();
    settled.sort_by_key(|r| std::cmp::Reverse(r.created));
    if work && !settled.is_empty() {
        let cards = settled.iter().take(6).map(|r| run_card(r)).collect::<Vec<_>>().join("\n");
        let total: f64 = runs.iter().filter_map(|r| r.savings.as_ref()).map(|x| x.saved_usd).sum();
        cands.push(Candidate { kind: "runs".into(), label: "the latest goal runs".into(), r#ref: "runs:latest".into(), text: format!("{cards}\n\nsaved by the router and MIN across every run: ${total:.2} (API-equivalent)"), base: 2 });
    }
    for r in settled.iter().take(40) {
        let card = run_card(r);
        cands.push(Candidate { kind: "run".into(), label: format!("goal run #{}", r.id), r#ref: format!("run:{}", r.id), text: card, base: 0 });
    }
    // the codebase intel: antiIDE's deterministic bundle for the question, small budget
    if about_code(&ask) || pins.iter().any(|p| p == "intel") {
        let conn = state.pick(); let q = ask.clone();
        let bundle = tokio::task::spawn_blocking(move || {
            let c = conn.lock().unwrap_or_else(|p| p.into_inner());
            antiloki_rs::codebase_smart_context_bundle::smartContextBundle(&json!({ "question": q, "token_budget": 1500, "coverage": true }), &c).ok()
        }).await.ok().flatten();
        if let Some(bd) = bundle {
            let md = s(bd.get("bundle_md")); let scope = s(bd.get("scope"));
            if scope != "none" && !md.trim().is_empty() && !md.contains("No codebase documentation yet") {
                cands.push(Candidate { kind: "intel".into(), label: format!("the codebase intel (antiIDE, scope {scope})"), r#ref: "intel".into(), text: md, base: 2 });
            }
        }
    }
    compose(&ask, cands, pins, drops, cap, 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keywords_split_paths_and_drop_stop_words() {
        assert_eq!(keywords("What does src/cart.ts do, and is it tested?"), vec!["src/cart.ts", "src", "cart", "tested"]);
        assert_eq!(score("the useCart hook in src/cart.ts has no tests", &keywords("is cart.ts tested")), 2, "cart.ts (whole, lowercase) + tested… 'tested' ⊂ 'tests'? no — cart.ts + cart");
        assert!(about_work("how much did the router save today?") && about_work("what is the QA verdict of the last plan?") && !about_work("hello there"));
        assert!(about_code("why is src/cart.ts coupled?") && about_code("which files have no test coverage") && !about_code("how was your day"));
    }
    #[test]
    fn compose_ranks_pins_first_then_matches_and_caps() {
        let cands = vec![
            Candidate { kind: "agent".into(), label: "cart totals".into(), r#ref: "agent:4".into(), text: "**operator:** fix the cart totals in src/cart.ts\n\n**agent:** done, useCart now memoizes".into(), base: 0 },
            Candidate { kind: "agent".into(), label: "checkout".into(), r#ref: "agent:3".into(), text: "**operator:** list the files".into(), base: 0 },
            Candidate { kind: "runs".into(), label: "the latest goal runs".into(), r#ref: "runs:latest".into(), text: "- goal run #38 · done".into(), base: 2 },
        ];
        let (md, used) = compose("what changed in the cart totals?", cands.clone(), &[], &[], 8000, 2);
        assert_eq!(used.iter().map(|u| u.r#ref.as_str()).collect::<Vec<_>>(), vec!["agent:4", "runs:latest"], "cart totals matches cart + totals; checkout none; the runs card rides on its base");
        assert!(md.starts_with("## Context antiloki looked up") && md.contains("read-only") && md.contains("### cart totals — 2 matches"));
        // a drop never rides, a pin always does — and pins come first
        let again = || vec![
            Candidate { kind: "agent".into(), label: "cart totals".into(), r#ref: "agent:4".into(), text: "**operator:** fix the cart totals in src/cart.ts".into(), base: 0 },
            Candidate { kind: "agent".into(), label: "checkout".into(), r#ref: "agent:3".into(), text: "**operator:** list the files".into(), base: 0 },
        ];
        let (_, used) = compose("cart totals", again(), &["agent:3".into()], &["agent:4".into()], 8000, 2);
        assert_eq!(used.iter().map(|u| (u.r#ref.as_str(), u.why.as_str())).collect::<Vec<_>>(), vec![("agent:3", "pinned")]);
        // nothing matches → nothing attached
        let (md, used) = compose("hello there", again(), &[], &[], 8000, 2);
        assert!(md.is_empty() && used.is_empty());
        // the cap trims the section that does not fit
        let big = vec![Candidate { kind: "agent".into(), label: "big".into(), r#ref: "agent:9".into(), text: "cart totals ".repeat(500), base: 0 }];
        let (md, used) = compose("cart totals", big, &[], &[], 1000, 1);
        assert!(used[0].chars <= 1000 && md.ends_with('…'));
    }

    // ── PHASE 5 (PLAN-THE-TESTS): the retriever's rules at their edges, and the route over a real (small) workspace ──
    fn cand(kind: &str, r: &str, text: &str, base: usize) -> Candidate { Candidate { kind: kind.into(), label: r.into(), r#ref: r.into(), text: text.into(), base } }
    fn refs(used: &[Pick]) -> Vec<&str> { used.iter().map(|u| u.r#ref.as_str()).collect() }

    #[test]
    fn keywords_are_lowercase_deduplicated_at_least_three_bytes_and_paths_ride_whole_and_split() {
        assert_eq!(keywords(""), Vec::<String>::new());
        assert_eq!(keywords("   ...  --  "), Vec::<String>::new());
        assert_eq!(keywords("Cart CART cart"), vec!["cart"], "case-folded and deduplicated");
        assert_eq!(keywords("is it ok to go?"), Vec::<String>::new(), "two-letter words and stop words carry nothing");
        assert_eq!(keywords("the cat sat"), vec!["cat", "sat"], "three letters is enough when it is not a stop word");
        assert_eq!(keywords("look at (src/a-b_c.tsx), then"), vec!["look", "src/a-b_c.tsx", "src", "tsx"], "punctuation trimmed, the path whole AND split, one-letter parts dropped");
        assert_eq!(keywords("./README.md?"), vec!["readme.md", "readme"], "leading ./ and trailing ? are trimmed; the extension is too short to be a word");
        assert_eq!(keywords("ão é"), vec!["ão"], "the floor is three BYTES: a two-letter accented word passes, a one-letter one does not");
    }

    #[test]
    fn a_score_counts_each_keyword_once_ignoring_case_and_empty_keywords_score_nothing() {
        let kws = vec!["cart".to_string(), "total".to_string(), "missing".to_string()];
        assert_eq!(score("CART cart Cart TOTAL", &kws), 2, "repeats do not stack");
        assert_eq!(score("cartography", &kws), 1, "a keyword is matched as a substring");
        assert_eq!(score("nothing here", &kws), 0);
        assert_eq!(score("anything", &[]), 0);
        assert_eq!(score("", &kws), 0);
    }

    #[test]
    fn the_work_and_code_detectors_read_words_and_paths() {
        for yes in ["how much did that plan cost", "what did the router SAVE", "any subagent running?", "spent $5", "the QA verdict", "status of the task"] { assert!(about_work(yes), "{yes}"); }
        for no in ["hello", "explain closures", ""] { assert!(!about_work(no), "{no}"); }
        for yes in ["open src/main.rs", "what is in app.tsx", "is there a bug here", "review the module", "why is the graph cyclic", "check README.md", "the Function is big"] { assert!(about_code(yes), "{yes}"); }
        for no in ["hello there", "tell me a joke", "", "the codec is slow"] { assert!(!about_code(no), "{no}: a code word must be a whole word"); }
        assert!(about_code("look at notes.txt/backup"), "any slash is a path");
        assert!(!about_code("version 1.5 is out"), "a dot alone is not a source file");
    }

    #[test]
    fn a_candidate_needs_the_minimum_score_to_ride_unless_it_is_pinned() {
        let c = || vec![cand("agent", "agent:1", "the cart page", 0), cand("agent", "agent:2", "the cart totals page", 0)];
        let (_, used) = compose("cart totals", c(), &[], &[], 8000, 2);
        assert_eq!(refs(&used), vec!["agent:2"], "one match (cart) is below the minimum of two");
        let (_, used) = compose("cart totals", c(), &[], &[], 8000, 1);
        assert_eq!(refs(&used), vec!["agent:2", "agent:1"], "at minimum one, the better match still comes first");
        assert_eq!(used[0].why, "2 matches");
        assert_eq!(used[1].why, "1 match", "singular for one");
        // a pinned candidate rides with no match at all, ahead of a better-scoring one
        let (md, used) = compose("cart totals", vec![cand("agent", "agent:3", "unrelated", 0), cand("agent", "agent:2", "the cart totals page", 0)], &["agent:3".into()], &[], 8000, 2);
        assert_eq!(used.iter().map(|u| (u.r#ref.as_str(), u.why.as_str())).collect::<Vec<_>>(), vec![("agent:3", "pinned"), ("agent:2", "2 matches")]);
        assert!(md.find("### agent:3").unwrap() < md.find("### agent:2").unwrap());
    }

    #[test]
    fn a_drop_beats_a_pin_and_equal_scores_keep_the_order_they_arrived_in() {
        let (md, used) = compose("cart totals", vec![cand("agent", "agent:1", "cart totals", 0)], &["agent:1".into()], &["agent:1".into()], 8000, 2);
        assert!(md.is_empty() && used.is_empty(), "the operator's drop wins over a pin on the same reference");
        let (_, used) = compose("cart totals", vec![cand("agent", "agent:5", "cart totals", 0), cand("agent", "agent:4", "cart totals", 0), cand("agent", "agent:6", "totals cart", 0)], &[], &[], 8000, 2);
        assert_eq!(refs(&used), vec!["agent:5", "agent:4", "agent:6"], "a stable sort");
        assert!(compose("cart", vec![], &[], &[], 8000, 1).0.is_empty());
        assert!(compose("", vec![cand("agent", "agent:1", "text", 5)], &[], &[], 8000, 2).1.len() == 1, "a base score alone can carry a candidate even for an empty ask");
    }

    #[test]
    fn the_cap_is_a_budget_and_a_section_that_does_not_fit_is_trimmed_or_skipped() {
        let text = |n: usize| format!("cart totals {}", "x".repeat(n));
        // 700 + 700 against a cap of 1000: the first fits whole, the second is trimmed into the 300 left... which is under 400, so it is skipped
        let (md, used) = compose("cart totals", vec![cand("a", "agent:1", &text(688), 0), cand("a", "agent:2", &text(688), 0)], &[], &[], 1000, 2);
        assert_eq!(refs(&used), vec!["agent:1"], "under 400 chars left, nothing more is attached");
        assert!(used[0].chars == 700 && !md.contains('…'));
        // with room for 400+ the second is trimmed with an ellipsis and the total never exceeds the cap
        let (md, used) = compose("cart totals", vec![cand("a", "agent:1", &text(388), 0), cand("a", "agent:2", &text(688), 0)], &[], &[], 1000, 2);
        assert_eq!(refs(&used), vec!["agent:1", "agent:2"]);
        assert!(used.iter().map(|u| u.chars).sum::<usize>() <= 1000 && md.ends_with('…'), "{:?}", used.iter().map(|u| u.chars).collect::<Vec<_>>());
        // characters, not bytes: an accented text is cut on a character boundary
        let (md, _) = compose("cart totals", vec![cand("a", "agent:1", &format!("cart totals {}", "ç".repeat(2000)), 0)], &[], &[], 1000, 2);
        assert!(md.ends_with('…') && md.is_char_boundary(md.len()));
        // the header explains the mode: read-only, no code changes
        assert!(compose("cart totals", vec![cand("a", "agent:1", "cart totals", 0)], &[], &[], 1000, 2).0.contains("You are antiChat: read-only"));
    }

    fn a_run(id: i64, goal: &str, status: &str, cwd: &str) -> GoalRun {
        serde_json::from_value(json!({ "id": id, "parent": "p", "goal": goal, "engine": "claude-code", "cwd": cwd, "write": false, "concurrency": 1, "status": status, "created": 1000 + id, "tasks": [], "blackboard": [], "events": [], "delivered": false, "router": true, "min": false, "plan_args": { "n": 4, "router": true, "auto": false, "always": false }, "stages": [], "qa_double": false, "qa_review_wanted": false, "qa_baseline_pending": false, "mid_task": false })).unwrap()
    }

    #[test]
    fn a_run_card_says_status_tasks_box_route_money_qa_and_the_first_line_of_the_report() {
        let mut r = a_run(38, "rewrite the parser\nwith a second line", "done", "/home/me/dev/shop");
        assert_eq!(run_card(&r), "- goal run #38 · done · box shop\n  rewrite the parser");
        r.tasks = serde_json::from_value(json!([{ "id": "t1", "title": "t", "prompt": "p", "owns": [], "needs": [], "acceptance": [], "status": "done", "wave": 0, "attempts": 1, "started": null, "ended": null, "report": null, "verdict": null, "error": null, "trace": null, "changed": [] }])).unwrap();
        assert!(run_card(&r).contains(" · 1 task ·"), "singular");
        r.tasks.push(r.tasks[0].clone());
        assert!(run_card(&r).contains(" · 2 tasks ·"), "plural");
        r.savings = Some(crate::goals::Savings { tokens_in: 0, tokens_out: 0, actual_usd: 0.5, baseline_usd: 1.75, saved_usd: 1.25, saved_pct: 71.0, baseline: String::new(), tiers: vec![] });
        r.route = serde_json::from_value(json!({ "tier": "mid", "delegate": true, "why": "w" })).ok();
        r.qa_review = Some(crate::goals::QaReview { passed: false, score: 61, summary: "t2 drifted".into(), findings: vec![], by: "judge".into(), t: 1, error: None });
        r.report = Some("First line of the report\nsecond".into());
        let c = run_card(&r);
        assert!(c.contains("$0.500 (saved $1.250)") && c.contains("route: tier") && c.contains("delegated"), "{c}");
        assert!(c.contains("QA FAILED 61/100 — t2 drifted") && c.ends_with("report: First line of the report"), "{c}");
        r.qa_review.as_mut().unwrap().passed = true; r.qa_review.as_mut().unwrap().summary.clear();
        assert!(run_card(&r).contains("QA passed 61/100") && !run_card(&r).contains("61/100 —"));
        // an inline answer says "inline"; a box with a trailing slash has no name; a long goal is cut to 140
        let inline = a_run(3, &"g".repeat(300), "inline", "/x/");
        let c = run_card(&inline);
        assert!(c.contains("· inline ·") && c.contains("box \n") && c.lines().nth(1).unwrap().trim().chars().count() == 140, "{c}");
    }

    // ── the route and the gatherer, over a database with the two tables they read ──
    fn scratch(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("antiloki-chat-auto-{tag}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".antiloki")).unwrap();
        d
    }
    fn log_of(root: &std::path::Path, pane: i64, turns: &[(&str, &str)]) {
        let lines: Vec<String> = turns.iter().enumerate().map(|(i, (who, text))| if *who == "you" {
            json!({ "seq": i + 1, "t": 1, "ev": { "type": "antiloki", "subtype": "user", "text": text } }).to_string()
        } else { json!({ "seq": i + 1, "t": 1, "ev": { "type": "assistant", "message": { "content": [{ "type": "text", "text": text }] } } }).to_string() }).collect();
        std::fs::write(crate::agent::events_read_path(&root.to_string_lossy(), pane), lines.join("\n")).unwrap();
    }
    /// (id, session, kind, worktree, settings)
    fn workspace(panes: &[(i64, i64, &str, Option<&std::path::PathBuf>, Value)]) -> Arc<AppState> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE cockpit_sessions (id INTEGER PRIMARY KEY, name TEXT, status TEXT);
            CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY, session_id INTEGER, pane_kind TEXT, worktree_path TEXT, settings_json TEXT, status TEXT);
            INSERT INTO cockpit_sessions VALUES (1, 'shop', 'open'), (2, 'archive', 'closed');").unwrap();
        for (id, sid, kind, wt, settings) in panes {
            conn.execute("INSERT INTO cockpit_panes (id, session_id, pane_kind, worktree_path, settings_json, status) VALUES (?1, ?2, ?3, ?4, ?5, 'idle')", rusqlite::params![id, sid, kind, wt.map(|p| p.to_string_lossy().to_string()), settings.to_string()]).unwrap();
        }
        crate::dispatch::test_app_state(conn)
    }
    fn goals() -> Arc<Goals> { crate::goals::Goals::new(crate::goals::scripted_prompts_for_queue().0, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 5) }
    async fn json_of(r: Response) -> (StatusCode, Value) {
        let st = r.status();
        let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        (st, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn every_seat_s_chat_can_be_attached_not_three_of_the_five() {
        // the same mistake agents/live made (a 3-list of engines) would leave the Cursor, Gemini and Grok agents' chats
        // out of antiChat's context; the one engine list is chat_pane::ENGINES
        let dirs: Vec<std::path::PathBuf> = (0..6).map(|i| scratch(&format!("seat{i}"))).collect();
        let kinds = crate::chat_pane::ENGINES;
        let mut panes = vec![];
        for (i, k) in kinds.iter().enumerate() {
            log_of(&dirs[i], 10 + i as i64, &[("you", "fix the cart totals rounding"), ("agent", "the cart totals now round half up")]);
            panes.push((10 + i as i64, 1, *k, Some(&dirs[i]), json!({})));
        }
        let st = workspace(&panes);
        let (md, used) = compose_for(&st, &goals(), "what happened to the cart totals?", 30_000, &[], &[], None).await;
        let got: Vec<String> = used.iter().map(|u| u.r#ref.clone()).collect();
        for (i, k) in kinds.iter().enumerate() { assert!(got.contains(&format!("agent:{}", 10 + i)), "the {k} pane's chat is missing from {got:?}"); }
        assert!(md.contains("the chat of the agent shop"));
        for d in dirs { let _ = std::fs::remove_dir_all(d); }
    }

    #[tokio::test]
    async fn only_open_panes_with_a_box_and_a_conversation_are_candidates_and_the_asking_chat_never_rides() {
        let (a, b, c, e) = (scratch("a"), scratch("b"), scratch("c"), scratch("e"));
        log_of(&a, 1, &[("you", "the cart totals are wrong"), ("agent", "fixed the cart totals")]);
        log_of(&b, 2, &[("you", "the cart totals are wrong"), ("agent", "fixed the cart totals")]);
        log_of(&c, 3, &[("you", "the cart totals are wrong"), ("agent", "fixed the cart totals")]);
        std::fs::write(crate::agent::events_read_path(&e.to_string_lossy(), 5), "").unwrap(); // an empty log
        let st = workspace(&[
            (1, 1, "claude-code", Some(&a), json!({ "recipe": { "name": "Cartwright" } })),
            (2, 1, "shell", Some(&b), json!({})),                 // not an agent
            (3, 2, "codex", Some(&c), json!({})),                 // its session is closed
            (4, 1, "codex", None, json!({})),                     // no box
            (5, 1, "codex", Some(&e), json!({})),                 // nothing said yet
            (6, 1, "codex", Some(&a), json!({})),                 // a second agent sharing the first one's box (own log, none written)
        ]);
        let (_, used) = compose_for(&st, &goals(), "the cart totals please", 30_000, &[], &[], None).await;
        assert_eq!(used.iter().map(|u| (u.r#ref.as_str(), u.label.as_str())).collect::<Vec<_>>(), vec![("agent:1", &*format!("the chat of the agent Cartwright (box {})", a.file_name().unwrap().to_string_lossy()))], "{used:?}");
        let (md, used) = compose_for(&st, &goals(), "the cart totals please", 30_000, &[], &[], Some(1)).await;
        assert!(used.is_empty() && md.is_empty(), "the asking chat's own log is the conversation, not context");
        // a pin rides even when the ask matches nothing, and a drop never does
        let (_, used) = compose_for(&st, &goals(), "hello", 30_000, &["agent:1".into()], &[], None).await;
        assert_eq!(refs(&used), vec!["agent:1"]);
        let (_, used) = compose_for(&st, &goals(), "the cart totals please", 30_000, &[], &["agent:1".into()], None).await;
        assert!(used.is_empty());
        for d in [a, b, c, e] { let _ = std::fs::remove_dir_all(d); }
    }

    #[tokio::test]
    async fn goal_runs_ride_only_when_the_ask_is_about_work_and_inline_answers_are_not_runs() {
        let st = workspace(&[]);
        let g = goals();
        const PLAN: &str = "{\"tier\":\"mid\",\"delegate\":true,\"why\":\"w\",\"tasks\":[{\"id\":\"t1\",\"title\":\"a\",\"tier\":\"high\",\"owns\":[\"src/a.ts\"],\"specialists\":[\"axis:defects\"]}]}";
        let (run_fn, _, _) = crate::goals::scripted_prompts_for_queue_with(vec![PLAN]);
        let g2 = crate::goals::Goals::new(run_fn, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 5);
        let dir = scratch("runs");
        let r = g2.plan_full("p", "add the export button to the report page", crate::cli::Engine::ClaudeCode, &dir.to_string_lossy(), false, 4, None, None, false, false).await;
        assert_ne!(r.status, crate::goals::RunStatus::Inline, "a planned run is a run, whatever state the scripted seat has taken it to");
        // about work: the latest-runs card (base 2) and the run's own card ride
        let (md, used) = compose_for(&st, &g2, "what is the status of the export button report?", 30_000, &[], &[], None).await;
        assert!(refs(&used).contains(&"runs:latest") && refs(&used).contains(&format!("run:{}", r.id).as_str()), "{used:?}");
        assert!(md.contains("saved by the router and MIN across every run: $") && md.contains("(API-equivalent)"), "{md}");
        // not about work: only the run whose card the ask names (two keywords) could ride; a plain question attaches nothing
        let (md, used) = compose_for(&st, &g2, "tell me a joke", 30_000, &[], &[], None).await;
        assert!(used.is_empty() && md.is_empty());
        // a workspace with no runs has nothing to attach even when the ask is about work
        let (_, used) = compose_for(&st, &g, "what is the status of the plan?", 30_000, &[], &[], None).await;
        assert!(used.is_empty());
        g2.cancel(r.id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_route_needs_an_ask_clamps_the_cap_and_reports_what_it_attached() {
        let st = workspace(&[]);
        for b in [json!({}), json!({ "ask": "   " }), json!({ "ask": 5 })] {
            let (code, v) = json_of(auto_context(st.clone(), goals(), Bytes::from(b.to_string())).await).await;
            assert_eq!((code, v["error"]["message"].as_str()), (StatusCode::BAD_REQUEST, Some("ask required")), "{b}");
        }
        let (code, _) = json_of(auto_context(st.clone(), goals(), Bytes::from("not json")).await).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        // a plain question: ok, nothing attached, and the answer still says so
        let (code, v) = json_of(auto_context(st, goals(), Bytes::from(json!({ "ask": "hello there" }).to_string())).await).await;
        assert_eq!((code, v["ok"].clone(), v["md"].clone(), v["used"].clone(), v["chars"].clone()), (StatusCode::OK, json!(true), json!(""), json!([]), json!(0)));
        assert!(v["ms"].is_u64());
        // cap: a nonsense cap is pulled into 1000..30000 (a cap of 1 would otherwise attach nothing: under 400 left)
        let d = scratch("cap");
        log_of(&d, 1, &[("you", "the cart totals are wrong"), ("agent", &"the cart totals were fixed. ".repeat(80))]);
        let st = workspace(&[(1, 1, "claude-code", Some(&d), json!({}))]);
        let (_, v) = json_of(auto_context(st.clone(), goals(), Bytes::from(json!({ "ask": "the cart totals", "cap": 1 }).to_string())).await).await;
        assert_eq!(v["used"].as_array().unwrap().len(), 1, "cap 1 is raised to 1000: {v}");
        let (_, v) = json_of(auto_context(st.clone(), goals(), Bytes::from(json!({ "ask": "the cart totals", "cap": 1_000_000_000u64, "pins": ["agent:1", 7], "drops": "no" }).to_string())).await).await;
        assert_eq!(v["used"][0]["why"], "pinned", "non-string pins are ignored, string ones honoured; a non-array drops is none");
        assert!(v["chars"].as_i64().unwrap() > 0);
        let (_, v) = json_of(auto_context(st, goals(), Bytes::from(json!({ "ask": "the cart totals", "drops": ["agent:1"] }).to_string())).await).await;
        assert_eq!(v["used"], json!([]));
        let _ = std::fs::remove_dir_all(&d);
    }
}
