//! CHATS AS PANES (operator 2026-09-21: "the old model is the chat sessions as independent messages to
//! OpenRouter — I want the chats to behave the same as the agents: a stream with a provider like Claude
//! Code, Codex, OpenCode. The difference is that it is read-only, has full access to any context if needed,
//! uses the graph RAG, and the same chat can be converted into another provider").
//!
//! A chat is a cockpit pane with `settings.chat = true` in the workspace's "chats" cockpit session:
//! * its box is `~/.antiloki/chats/<pane_id>/` — a state directory, never a worktree (there is nothing to
//!   write); the engine reads the workspace through `--add-dir` (Claude), the read-only sandbox (Codex) or
//!   its read tool (OpenCode). The pack, MCP and event log live in that directory like any box's.
//! * READ-ONLY is enforced at the tool layer, per engine (`harden`, `claude_readonly_args`, the Codex
//!   sandbox in agent_codex.rs) — never a sentence in the prompt.
//! * each send rides the retriever's picks in front of the ask (`before_send` → chat_auto): the other
//!   agents' chats, the runs, the codebase intel — the picks are emitted as a `context` event for the chips.
//! * EPISODES (operator: "episodes are not chat sessions — they are consolidations in time of the same chat
//!   session, like a history"): a boundary (New topic · a 4h gap · size) writes an `episode` marker with the
//!   closed episode's compact snapshot — its record, its name — and the engine session simply goes on. Only a
//!   PROVIDER SWITCH restarts the engine: the current episode is consolidated, a new one opens on the other
//!   engine (each episode has one provider), and the conversation so far rides in front of the NEXT message
//!   — no seed turn, no wasted reply.
use crate::agent::AgentRegistry;
use crate::dispatch::{engine_call, AppState};
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const SESSION_NAME: &str = "chats";
/// THE ENGINES A CHAT MAY BE SWITCHED TO — every seat, since 2026-09-28 (`CHAT_ENGINES` below is the same list as
/// `cli::Engine`; this is its string form, for the switch's own validation and for the picker the FE draws).
pub const ENGINES: [&str; 6] = ["claude-code", "codex", "opencode", "gemini", "grok", "cursor"];
/// the episode rules — the same numbers antiChat used for its episodes (chat-thread.tsx, 2026-07-27)
pub const GAP_MS: i64 = 4 * 3600 * 1000;
pub const HARD_TURNS: usize = 120;
pub const SOFT_TURNS: usize = 60;
pub const PAUSE_MS: i64 = 20 * 60 * 1000;
const HANDOFF_CAP: usize = 24_000;
const SNAPSHOT_TURNS: usize = 8;
const SNAPSHOT_TURN_CHARS: usize = 600;

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn num(v: Option<&Value>) -> i64 { v.and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0) }
fn body_json(b: &Bytes) -> Value { serde_json::from_slice(b).unwrap_or(Value::Null) }
pub(crate) fn settings_of(pane: &Value) -> Value { pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).or_else(|| pane.get("settings").cloned()).unwrap_or(json!({})) }
pub(crate) fn ctx() -> Value { json!({ "actor": "web-fe", "source": "web-fe" }) }

/// Is this pane a chat? (`settings.chat`)
pub fn is_chat(pane: &Value) -> bool { settings_of(pane).get("chat").and_then(Value::as_bool).unwrap_or(false) }
/// READ-ONLY IS ITS OWN FLAG (operator 2026-09-27: "first is not suppose to be read-only, we dont have this anymore").
/// `chat` used to mean both "antiChat's chrome + episodes" and "cannot write"; main and its forks want the first
/// without the second. An explicit `settings.readonly` wins; a plain chat with no say stays read-only as before.
pub fn is_readonly(pane: &Value) -> bool {
    // FRONT MODE (operator 2026-09-29: "read-only enforcements are the wrong tool… antichat is a layer that gathers context, hands it to Heimdall and
    // renders the answer"): a chat is the VOICE. Whatever `settings.readonly` says, its session does not write - writes are Heimdall runs, in a worktree,
    // with a scope, a verifier and a stats box. `settings.front.enabled = false` gives a chat back its old say.
    if is_front(pane) { return true }
    settings_of(pane).get("readonly").and_then(Value::as_bool).unwrap_or_else(|| is_chat(pane))
}
/// Is this pane in FRONT MODE? A chat is, unless `settings.front.enabled` says false.
pub fn is_front(pane: &Value) -> bool { is_chat(pane) && settings_of(pane).get("front").and_then(|f| f.get("enabled")).and_then(Value::as_bool).unwrap_or(true) }
/// `~/.antiloki/chats` (or `ANTILOKI_CHATS_DIR`)
pub fn chats_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ANTILOKI_CHATS_DIR") { return PathBuf::from(d); }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".antiloki").join("chats")
}
/// A CHAT'S FOLDER BELONGS TO ITS WORKSPACE (2026-10-11, the operator: "I opened antiloki and it showed a continuation of the same conversation in all projects"). The folder was
/// `chats/<pane id>`, and pane ids are numbered per workspace database: project A's chat 5 and project B's chat 5 were the same folder, so the same `agent-events-5.jsonl`, the same
/// episode markers, the same CLI session. The folder is now `chats/<workspace key>/<pane id>`. The key is the workspace folder's name plus a hash of its full path, so two
/// projects called `app` do not meet. Pure - tested.
pub fn workspace_key(root: &str) -> String {
    let norm = root.trim().trim_end_matches(['/', '\\']).replace('\\', "/").to_lowercase();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in norm.bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); }
    let name: String = norm.rsplit('/').next().unwrap_or("").chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).take(24).collect();
    format!("{}-{:08x}", if name.is_empty() { "ws" } else { &name }, h & 0xffff_ffff)
}
pub fn chat_dir(workspace_root: &str, pane_id: i64) -> PathBuf { chats_dir().join(workspace_key(workspace_root)).join(pane_id.to_string()) }
/// The folder the chats used before they were scoped: shared by every workspace that had a pane with this id.
pub fn is_legacy_chat_dir(worktree_path: &str, pane_id: i64) -> bool { !worktree_path.is_empty() && Path::new(worktree_path) == chats_dir().join(pane_id.to_string()) }
/// A chat made before the folders were scoped is given its own empty folder in its workspace; the old shared one is left exactly as it was (nothing is deleted - it holds
/// every workspace's turns interleaved, and no one of them can claim it). Returns the pane's new folder.
async fn adopt_legacy_chat(state: &Arc<AppState>, root: &str, pane_id: i64) -> Option<String> {
    let dir = chat_dir(root, pane_id);
    std::fs::create_dir_all(dir.join(".antiloki")).ok()?;
    harden(&dir);
    let d = dir.to_string_lossy().to_string();
    set_worktree(state, pane_id, &d).await;
    Some(d)
}
/// Claude Code's read-only tool set: the read tools, the web, antiloki's own MCP tools; every write tool and
/// the shell are removed, and the workspace is an added directory so reads there never prompt. Pure.
pub fn claude_readonly_args(workspace_root: &str) -> Vec<String> {
    let mut a = vec![
        "--allowedTools".to_string(), "Read,Grep,Glob,LS,WebFetch,WebSearch,mcp__antiloki".to_string(),
        "--disallowedTools".to_string(), "Edit,Write,MultiEdit,NotebookEdit,Bash".to_string(),
    ];
    if !workspace_root.trim().is_empty() { a.push("--add-dir".into()); a.push(workspace_root.to_string()); }
    a
}
/// The box's engine files for a chat: OpenCode's config denies edits and the shell (written before the pack
/// bakes, so its MCP and instructions patch in on top; re-hardened after, since the bake sets `bash`).
pub fn harden(cwd: &Path) {
    // 2026-10-13: a tracked opencode.json in a main checkout is the project's — do not rewrite it.
    if cwd.join(".git").is_dir() {
        let ok = crate::proc::cmd("git").args(["ls-files", "--error-unmatch", "opencode.json"]).current_dir(cwd).output().ok().filter(|o| o.status.success());
        if ok.is_some() { return; }
    }
    let p = cwd.join("opencode.json");
    let mut cfg: Value = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({ "$schema": "https://opencode.ai/config.json" }));
    if let Some(o) = cfg.as_object_mut() {
        let mut perm = o.get("permission").and_then(Value::as_object).cloned().unwrap_or_default();
        perm.insert("edit".into(), json!("deny")); perm.insert("bash".into(), json!("deny")); perm.insert("write".into(), json!("deny"));
        o.insert("permission".into(), Value::Object(perm));
    }
    let _ = std::fs::write(&p, serde_json::to_string_pretty(&cfg).unwrap_or_default());
    // 2026-10-13: a new untracked opencode.json in a main checkout must not dirty git status.
    if cwd.join(".git").is_dir() { antiloki_rs::cockpit_cockpit_pane_context_bake::git_exclude_line(cwd, "opencode.json"); }
}

// ── the episodes, read from the event log — pure over the lines ──────────────────────────────────
/// Text antiloki itself puts into an agent's input (new-team brief, provider takeover, a team's recall, audit and result notes): a turn, but never the person's ask.
pub(crate) fn is_internal_brief(t: &str) -> bool {
    let t = t.trim_start();
    if ["[antiloki]", "[team memory", "[team audit", "[team result", "[improver due"].iter().any(|p| t.starts_with(p)) { return true; }
    // a specialist's standing brief ("You are Reels Specialist, a specialist. Your purpose: ...") is the pane's instructions, not the person's ask (seen 2026-11-25: it named a saga)
    t.starts_with("You are ") && t.chars().take(160).collect::<String>().contains(", a specialist")
}
/// An ask as the person wrote it: a leading `<pasted_content ...>` wrapper (the paste tag some clients add) is not part of it.
pub(crate) fn strip_paste_tag(t: &str) -> String {
    let t = t.trim_start();
    if t.starts_with("<pasted_content") { if let Some(i) = t.find('>') { return t[i + 1..].trim_start().to_string(); } }
    t.to_string()
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Episode { pub n: usize, pub seq: u64, pub t: i64, pub reason: String, pub engine: String, pub turns: usize, pub last_t: i64, pub first_ask: String, pub snapshot: String,
    /// WHERE IT HAPPENED (2026-09-27) — the worktree, the agent slug that owned it, and the pane it was forked from
    /// if it was a fork's. Empty for every episode written before this existed, which is honest: we do not know.
    pub r#box: String, pub agent: String, pub forked_from: Option<i64>,
    /// THE CONSOLIDATION'S NAME (operator: "the episodes are consolidations, they can have titles and subtitles"):
    /// written on the marker that closed it; the open episode is named from its first ask and reply, live
    pub title: String, pub subtitle: String, pub replies: usize, pub first_reply: String,
    /// THE JOURNEY (operator 2026-09-23: "a new episode consolidates the current one into a summary and a journey story
    /// snapshot"): the engine's short narrative of the closed episode, beside the deterministic transcript snapshot
    pub story: String }
/// Split the log at the `episode` markers. Episode 1 starts at the first line; each marker opens the next.
pub fn episodes_of<'a>(lines: impl Iterator<Item = &'a str>, engine_now: &str) -> Vec<Episode> {
    let mut out: Vec<Episode> = vec![];
    let mut cur: Option<Episode> = None;
    let mut first_engine: Option<String> = None;
    // THE SAME EVENT, TWICE, IS ONE EVENT (2026-09-27). pane 25's log holds 30 lines with 16 distinct seqs, so every
    // count here doubled: the sidebar read "4 turns · 6 replies" over two asks and three replies. The writer's
    // duplication is its own bug; this makes the counts true meanwhile, and a seq is what identifies an event.
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for line in lines {
        let Ok(w) = serde_json::from_str::<Value>(line) else { continue };
        let (seq, t) = (w.get("seq").and_then(Value::as_u64).unwrap_or(0), w.get("t").and_then(Value::as_i64).unwrap_or(0));
        if !seen.insert(seq) { continue; }
        let ev = w.get("ev").cloned().unwrap_or(Value::Null);
        let ty = s(ev.get("type")); let sub = s(ev.get("subtype"));
        if ty == "antiloki" && (sub == "started" || sub == "resumed") && first_engine.is_none() { first_engine = Some(s(ev.get("engine")).trim().to_string()).filter(|e| !e.is_empty()); }
        if ty == "antiloki" && sub == "episode" {
            // an episode without a `started` engine of its own: the marker that closed it says what it ran on
            // an episode nothing was said in (a provider switch right after another) is REPLACED by this marker,
            // not closed: the timeline never shows an empty chapter; the record it carried (the previous real
            // episode's) rides on to the replacement so that episode keeps its record
            let mut carried: Option<String> = None;
            if let Some(mut c) = cur.take() {
                if c.turns == 0 && c.replies == 0 { carried = Some(c.snapshot); }
                else {
                    if c.engine.is_empty() { c.engine = s(ev.get("from_engine")); }
                    let (ct, cs) = (s(ev.get("closed_title")), s(ev.get("closed_subtitle")));
                    if !ct.is_empty() { c.title = ct; } if !cs.is_empty() { c.subtitle = cs; }
                    out.push(c);
                }
            }
            let snapshot = match carried { Some(prev) if !prev.trim().is_empty() => prev, _ => s(ev.get("snapshot")) };
            cur = Some(Episode { n: out.len() + 1, seq, t, reason: s(ev.get("reason")), engine: s(ev.get("engine")), turns: 0, last_t: t, first_ask: String::new(), snapshot, title: String::new(), subtitle: String::new(), replies: 0, first_reply: String::new(), story: String::new(),
                r#box: s(ev.get("box")), agent: s(ev.get("agent")), forked_from: ev.get("forked_from").and_then(Value::as_i64) });
            continue;
        }
        // the engine's name for a closed episode (`name_episode_later`) — it lands after the marker, keyed by n
        if ty == "antiloki" && sub == "episode_named" {
            let n = ev.get("n").and_then(Value::as_u64).unwrap_or(0) as usize;
            if let Some(e) = out.iter_mut().find(|e| e.n == n) { let (nt, ns, st) = (s(ev.get("title")), s(ev.get("subtitle")), s(ev.get("story"))); if !nt.trim().is_empty() { e.title = nt.trim().to_string(); } if !ns.trim().is_empty() { e.subtitle = ns.trim().to_string(); } if !st.trim().is_empty() { e.story = st.trim().to_string(); } }
            continue;
        }
        // EPISODE ONE PREDATES ITS OWN MARKER — it starts at the first line, not at a boundary, so there is nothing
        // to read a box off. Empty is the honest answer; inventing the pane's CURRENT box would claim a fork's first
        // episode happened somewhere it did not.
        let c = cur.get_or_insert_with(|| Episode { n: out.len() + 1, seq, t, reason: "the first".into(), engine: String::new(), turns: 0, last_t: t, first_ask: String::new(), snapshot: String::new(), title: String::new(), subtitle: String::new(), replies: 0, first_reply: String::new(), story: String::new(),
            r#box: String::new(), agent: String::new(), forked_from: None });
        if ty == "assistant" {
            let text = ev.get("message").and_then(|m| m.get("content")).and_then(Value::as_array).map(|a| a.iter().filter(|b| s(b.get("type")) == "text").map(|b| s(b.get("text"))).filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join(" ")).unwrap_or_default();
            if !text.trim().is_empty() { c.replies += 1; if c.first_reply.is_empty() { c.first_reply = text.trim().to_string(); } }
        }
        if c.engine.is_empty() && ty == "antiloki" && (sub == "started" || sub == "resumed") { c.engine = s(ev.get("engine")).trim().to_string(); }
        if ty == "antiloki" && sub == "user" { c.turns += 1; let t = strip_paste_tag(&s(ev.get("text"))); if c.first_ask.is_empty() && !is_internal_brief(&t) { c.first_ask = t.chars().take(120).collect(); } }   // antiloki's own briefs (new team, provider takeover) never name a saga: the person's first real ask does
        if ty == "antiloki" && sub == "user" || ty == "assistant" || ty == "result" { c.last_t = t; }
    }
    if let Some(c) = cur { out.push(c); }
    // the engine of an episode without a marker of its own: the first `started` line, else the pane's current
    let fallback = first_engine.unwrap_or_else(|| engine_now.to_string());
    for e in out.iter_mut() {
        if e.engine.is_empty() { e.engine = fallback.clone(); }
        // an episode not yet named by its marker (the open one, or an old log): its first ask and first reply
        if e.title.is_empty() { e.title = title_of(&e.first_ask); }
        if e.subtitle.is_empty() { e.subtitle = subtitle_of(&e.first_reply); }
    }
    out
}
/// A title from the first ask: one line, the first clause, ≤ 60 chars. Pure.
pub fn title_of(ask: &str) -> String {
    let one = ask.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut one = one.trim_start_matches(|c: char| !c.is_alphanumeric()).to_string();
    // a short framing prefix ("in one line:", "quick:") is not the title — what follows it is
    if let Some((pre, rest)) = one.split_once(": ") { if pre.split_whitespace().count() <= 3 && !rest.trim().is_empty() { one = rest.trim().to_string(); } }
    // the first clause: `?` `!` `:`, or a full stop followed by a space (a path's dot is not one)
    let ends = [one.find(['?', '!', ':']).map(|i| i + 1), one.find(". ").map(|i| i + 1)];
    let cut = ends.into_iter().flatten().min().unwrap_or(one.len()).min(one.len());
    let head = one[..cut].trim().to_string();
    if head.chars().count() > 60 { format!("{}…", head.chars().take(58).collect::<String>().trim_end()) } else { head }
}
/// A subtitle from the first reply: its first sentence, ≤ 110 chars, markdown stripped. Pure.
pub fn subtitle_of(reply: &str) -> String {
    let flat = reply.replace(['*', '`'], "").replace("\n#", "\n").replace("\n>", "\n").trim_start_matches(['#', '>', ' ']).split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = flat.match_indices(". ").next().map(|(i, _)| i + 1).unwrap_or(flat.len());
    let head = flat[..cut].trim().to_string();
    if head.chars().count() > 110 { format!("{}…", head.chars().take(108).collect::<String>().trim_end()) } else { head }
}
/// The closed episode's compact snapshot: its last turns, each trimmed — deterministic, $0. Pure.
pub fn snapshot_of<'a>(lines: impl Iterator<Item = &'a str>, from_seq: u64) -> String {
    let mut turns: Vec<String> = vec![];
    for line in lines {
        let Ok(w) = serde_json::from_str::<Value>(line) else { continue };
        if w.get("seq").and_then(Value::as_u64).unwrap_or(0) < from_seq { continue; }
        let ev = w.get("ev").cloned().unwrap_or(Value::Null);
        match s(ev.get("type")).as_str() {
            "antiloki" if s(ev.get("subtype")) == "user" => { let t = s(ev.get("text")); if !t.trim().is_empty() { turns.push(format!("- **you:** {}", trim_to(t.trim(), SNAPSHOT_TURN_CHARS))); } }
            "assistant" => {
                let text = ev.get("message").and_then(|m| m.get("content")).and_then(Value::as_array).map(|a| a.iter().filter(|b| s(b.get("type")) == "text").map(|b| s(b.get("text"))).filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                if !text.trim().is_empty() { turns.push(format!("- **antiloki:** {}", trim_to(text.trim(), SNAPSHOT_TURN_CHARS))); }
            }
            _ => {}
        }
    }
    let n = turns.len();
    let tail: Vec<String> = turns.into_iter().skip(n.saturating_sub(SNAPSHOT_TURNS)).collect();
    if tail.is_empty() { String::new() } else { format!("{} turn(s); the last {}:\n{}", n, tail.len(), tail.join("\n")) }
}
/// ONE EPISODE'S CONVERSATION, IN FULL (operator 2026-09-27: "the episodes are clickable they carry the history of
/// the conversation, we can see the conversation that happened on that episode onclick").
///
/// `snapshot_of` above answers a different question — it compacts the LAST few turns of an episode, trimmed, for a
/// prompt to carry cheaply. Clicking an episode wants the opposite: every turn it contained, untrimmed, bounded at
/// both ends. So this takes the episode's own `seq` and the NEXT episode's `seq` as the half-open range
/// `[from, to)` — which is exactly how `episodes_of` cut them, so the two can never disagree about where an
/// episode ends. `to = None` is the live episode: everything from `from` onward.
///
/// Untrimmed on purpose: this is the conversation the operator is asking to read, not a summary of it. Pure —
/// tested over a literal log.
pub fn turns_between<'a>(lines: impl Iterator<Item = &'a str>, from_seq: u64, to_seq: Option<u64>) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    // A SEQ IS UNIQUE, SO THE READER ENFORCES IT (2026-09-27). Found by reading the first episode back: the log of
    // pane 25 holds 30 lines carrying 16 distinct seqs — every event written twice — so the conversation rendered
    // twice and `episodes_of` counted "4 turns · 6 replies" for two asks and three replies. The writer is at fault
    // and is a separate bug; a reader that trusts its input to be well-formed turns one defect into several, and a
    // seq is the one thing in this file guaranteed to identify an event.
    let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for line in lines {
        let Ok(w) = serde_json::from_str::<Value>(line) else { continue };
        let seq = w.get("seq").and_then(Value::as_u64).unwrap_or(0);
        if seq < from_seq { continue; }
        // …and NOT `break` on the upper bound, for the same reason: a duplicated log is not monotonic, so an early
        // exit would end the episode at the first replayed line rather than at its real boundary.
        if let Some(to) = to_seq { if seq >= to { continue; } }
        if !seen.insert(seq) { continue; }
        let at = w.get("t").and_then(Value::as_i64).unwrap_or(0);
        let ev = w.get("ev").cloned().unwrap_or(Value::Null);
        match s(ev.get("type")).as_str() {
            "antiloki" if s(ev.get("subtype")) == "user" => {
                let text = s(ev.get("text"));
                if !text.trim().is_empty() { out.push(json!({ "role": "you", "text": text, "t": at, "seq": seq })); }
            }
            "assistant" => {
                let text = ev.get("message").and_then(|m| m.get("content")).and_then(Value::as_array)
                    .map(|a| a.iter().filter(|b| s(b.get("type")) == "text").map(|b| s(b.get("text")))
                        .filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                if !text.trim().is_empty() { out.push(json!({ "role": "antiloki", "text": text, "t": at, "seq": seq })); }
            }
            _ => {}
        }
    }
    out
}

/// THE CONVERSATION BRIEF (operator 2026-09-29: "the planners should always know the entire episode and the consolidated summary of the
/// previous episodes on the current agent"). The planner used to receive only the goal text, so a goal that leaned on the conversation - "go",
/// "the second one", "like we said" - was planned blind, and what the operator had already decided was asked again.
///
/// Two parts, both from the pane's own event log and both deterministic (no model summarises anything here, so it cannot drift):
///   · THE PREVIOUS EPISODES, CONSOLIDATED: each closed episode's title and the engine's short story of it (else the transcript snapshot),
///     newest kept first when they do not all fit, shown oldest to newest;
///   · THIS EPISODE, IN FULL: every turn the operator and the assistant took, each capped, in order. If the whole does not fit the budget the
///     MIDDLE goes - the opening (what the episode was about) and the latest turns (what "it" and "that" mean) are what a plan leans on.
/// Empty when there is no history. Pure over the lines - tested.
pub fn brief_of<'a>(lines: impl Iterator<Item = &'a str>, engine_now: &str, budget: usize) -> String {
    let all: Vec<&str> = lines.collect();
    let eps = episodes_of(all.iter().copied(), engine_now);
    let Some(cur) = eps.last() else { return String::new() };
    let one = |t: &str, n: usize| trim_to(t, n);
    // ── the previous episodes, consolidated
    let prior: Vec<&Episode> = eps[..eps.len() - 1].iter().filter(|e| e.turns > 0 || e.replies > 0).collect();
    let lines_of: Vec<String> = prior.iter().map(|e| {
        let name = if e.title.trim().is_empty() { one(&e.first_ask, 80) } else { one(&e.title, 80) };
        let sub = if e.subtitle.trim().is_empty() { String::new() } else { format!(" — {}", one(&e.subtitle, 100)) };
        let body = if !e.story.trim().is_empty() { one(&e.story, 700) } else if !e.snapshot.trim().is_empty() { one(&e.snapshot, 700) } else { String::new() };
        format!("- Episode {}: {name}{sub}{}\n", e.n, if body.is_empty() { String::new() } else { format!("\n    {body}") })
    }).collect();
    let prior_budget = budget * 4 / 10;
    let mut kept: Vec<&String> = vec![]; let mut used = 0usize;
    for l in lines_of.iter().rev() { if used + l.len() > prior_budget && !kept.is_empty() { break } used += l.len(); kept.push(l); }
    kept.reverse();
    let dropped = lines_of.len() - kept.len();
    let mut out = String::new();
    if !kept.is_empty() {
        out.push_str("EARLIER EPISODES OF THIS CONVERSATION (consolidated):\n");
        if dropped > 0 { out.push_str(&format!("  ({dropped} older episode{} not shown)\n", if dropped == 1 { "" } else { "s" })); }
        for l in kept { out.push_str(l); }
        out.push('\n');
    }
    // ── this episode, in full
    let turns = turns_between(all.iter().copied(), cur.seq, None);
    if !turns.is_empty() {
        let fmt = |t: &Value| { let who = if s(t.get("role")) == "you" { "operator" } else { "assistant" }; let cap = if who == "operator" { 700 } else { 900 }; format!("{who}: {}\n", one(&s(t.get("text")), cap)) };
        let rendered: Vec<String> = turns.iter().map(fmt).collect();
        let room = budget.saturating_sub(out.len()).max(1200);
        let total: usize = rendered.iter().map(String::len).sum();
        out.push_str(&format!("THIS EPISODE (#{}), {}:\n", cur.n, if total <= room { "in full".to_string() } else { "the opening and the latest turns".to_string() }));
        if total <= room { for r in &rendered { out.push_str(r); } }
        else {
            let head = 2.min(rendered.len()); let mut used = rendered[..head].iter().map(String::len).sum::<usize>(); let mut tail: Vec<&String> = vec![];
            for r in rendered[head..].iter().rev() { if used + r.len() > room { break } used += r.len(); tail.push(r); }
            tail.reverse();
            for r in &rendered[..head] { out.push_str(r); }
            let omitted = rendered.len() - head - tail.len();
            if omitted > 0 { out.push_str(&format!("  … {omitted} turn{} in the middle not shown …\n", if omitted == 1 { "" } else { "s" })); }
            for r in tail { out.push_str(r); }
        }
    }
    out
}
/// The pane's brief, read from its log. `budget` is in characters.
pub fn conversation_brief(cwd: &str, pane_id: i64, engine_now: &str, budget: usize) -> String {
    if pane_id <= 0 { return String::new() }
    brief_of(events_text(cwd, pane_id).lines(), engine_now, budget)
}

fn trim_to(t: &str, n: usize) -> String { let one = t.split_whitespace().collect::<Vec<_>>().join(" "); if one.chars().count() > n { format!("{}…", one.chars().take(n).collect::<String>()) } else { one } }
/// Which boundary, if any, a send hits — the rules of antiChat's episodes. Pure.
pub fn boundary_for(ep: &Episode, now: i64) -> Option<String> {
    if ep.turns == 0 { return None; }
    let gap = now - ep.last_t;
    if gap >= GAP_MS { return Some(format!("{}h gap", gap / 3_600_000)); }
    if ep.turns >= HARD_TURNS { return Some(format!("rotated at {} turns", ep.turns)); }
    if ep.turns >= SOFT_TURNS && gap >= PAUSE_MS { return Some(format!("rotated at {} turns, after a {}m pause", ep.turns, gap / 60_000)); }
    None
}
/// A SIZE boundary (the turn counts) restarts the engine in place — same provider, a fresh context window — so
/// a day-long chat never drags a bloated window; a gap or a New topic is a chapter only. Pure.
pub fn boundary_restarts(reason: &str) -> bool { reason.starts_with("rotated at") }
/// What rides in front of the next message after a provider switch or an in-place restart (a plain same-engine
/// consolidation carries nothing — the engine already has the conversation). Pure.
pub fn preamble(marker: &Value) -> String {
    let (snapshot, from, to) = (s(marker.get("snapshot")), s(marker.get("from_engine")), s(marker.get("engine")));
    let fresh = marker.get("fresh").and_then(Value::as_bool).unwrap_or(false);
    if snapshot.trim().is_empty() || to.is_empty() { return String::new(); }
    let rule = "You are antiloki's read-only chat: you answer from the workspace, antiloki's intel and the other agents' work; you never change files.";
    let head = if !from.is_empty() && from != to { format!("You are continuing a conversation that ran on {from} and now continues on {to}. {rule} The conversation so far:") }
        else if fresh { format!("You are continuing a conversation on {to} in a fresh context window (the previous one grew past its size). {rule} The conversation so far:") }
        else { return String::new() };
    format!("<previous-episode>\n{head}\n\n{snapshot}\n</previous-episode>\n\n")
}

// ── the DB side ───────────────────────────────────────────────────────────────────────────────────
pub(crate) async fn pane_row(state: &Arc<AppState>, pane_id: i64) -> Option<Value> {
    engine_call(state, "system.cockpit.pane.get", json!({ "id": pane_id }), ctx()).await.ok().map(|(v, _)| v).filter(|v| !v.is_null())
}
pub(crate) async fn chats_session(state: &Arc<AppState>) -> Result<i64, String> {
    let (list, _) = engine_call(state, "system.cockpit.session.get", json!({}), ctx()).await?;
    let found = list.get("sessions").and_then(Value::as_array).into_iter().flatten().filter(|x| s(x.get("name")) == SESSION_NAME && s(x.get("status")) != "closed").map(|x| num(x.get("id"))).filter(|id| *id > 0).min();
    if let Some(id) = found { return Ok(id); }
    let (made, _) = engine_call(state, "system.cockpit.session.create", json!({ "name": SESSION_NAME, "mission_md": "Conversations with antiloki — read-only chats over the workspace, its intel and the agents' work." }), ctx()).await?;
    let id = num(made.get("session").and_then(|x| x.get("id")));
    if id > 0 { Ok(id) } else { Err(format!("session/create: {}", { let e = s(made.get("error")); if e.is_empty() { "returned nothing".into() } else { e } })) }
}
/// THROUGH THE ACTION THAT OWNS THE TABLE (audit 2026-09-28). This wrote `cockpit_panes` with raw SQL — the
/// deviation the architecture rule exists to prevent, and the one that bit us: `system.cockpit.pane.update` does
/// more than set the columns, it re-bakes the on-disk brief, and a direct write silently skips that.
pub(crate) async fn set_worktree(state: &Arc<AppState>, pane_id: i64, dir: &str) {
    let _ = engine_call(state, "system.cockpit.pane.update",
        json!({ "pane_id": pane_id, "worktree_path": dir, "status": "ready" }), ctx()).await;
}
fn workspace_root(state: &Arc<AppState>) -> String {
    let conn = state.pick();
    let c = conn.lock().unwrap_or_else(|p| p.into_inner());
    antiloki_rs::engine_workspace_root::getWorkspaceRoot(&c).as_str().unwrap_or_default().to_string()
}
pub fn workspace_root_of(state: &Arc<AppState>) -> String { workspace_root(state) }

/// One agent's event log, as text. PER PANE since 2026-09-24 — see agent::events_path for why a shared file made
/// every agent of a workspace replay every other agent's turns.
pub(crate) fn events_text(cwd: &str, pane_id: i64) -> String {
    std::fs::read_to_string(crate::agent::events_read_path(cwd, pane_id)).unwrap_or_default()
}

/// Write the marker, restart the engine fresh (the next message carries the snapshot). `engine`: switch to it.
/// C0 — THE CHAMELEON CHANGES FORM (PLAN-THE-CHAMELEON).
///
/// One chat, one conversation, and the form underneath changes with the work. `roles::change_for` decides which of
/// the three things happens; this performs it:
///
///   · `Adapter` — a different role on the SAME engine. Nothing restarts. The adapter (globs, skills, specialists,
///     packs) swaps on the running session, which costs nothing. This is the common case and the reason the
///     chameleon is affordable at all.
///   · `Handoff` — a different engine. This is EXACTLY the provider switch `rotate` has always done: the episode
///     consolidates, a new one opens on the other engine, and the conversation rides in front of the next message.
///     No new machinery — the same path, triggered by the role instead of by the operator.
///   · `Stay` / `None` — nothing happens, and the reason is still returned so the run can say why it did not move.
///
/// Returns the decision, so the caller can log the sentence. A router that changes who is working without a word
/// is worse than one that does not route.
pub async fn change_form(state: &Arc<AppState>, reg: &Arc<AgentRegistry>, pane: &Value, route: &Value,
                         current_role: &str, current: Option<(f64, i64)>, next: Option<(f64, i64)>)
                         -> crate::roles::Change {
    let current_engine = s(pane.get("pane_kind"));
    let name = route.get("name").and_then(Value::as_str).unwrap_or(current_role).to_string();
    let change = hold_unreachable_handoff(crate::roles::change_for(current_role, &current_engine, route, current, next), &name,
                                          |k| crate::cli::engine_of(&json!(k)).map(|e| CHAT_ENGINES.contains(&e) && crate::cli::engine_usable(e)).unwrap_or(false));
    if let crate::roles::Change::Handoff { engine, why, .. } = &change {
        // the reason names the FORM, not the provider: "became Frontender" is what happened; the engine moving is
        // how it happened, and it is already in the episode marker.
        let name = route.get("name").and_then(Value::as_str).unwrap_or(current_role);
        let _ = rotate(state, reg, pane, &format!("became {name} — {why}"), Some(engine), false).await;
    }
    change
}

/// The engines a chat pane can run — `agent::start` speaks exactly these three.
/// EVERY SEAT, since 2026-09-28: Cursor and Grok through `agent_print`, Gemini through `agent_gemini`.
pub const CHAT_ENGINES: [crate::cli::Engine; 6] = [crate::cli::Engine::ClaudeCode, crate::cli::Engine::Codex,
    crate::cli::Engine::Opencode, crate::cli::Engine::Gemini, crate::cli::Engine::Grok, crate::cli::Engine::Cursor];

/// A DECLINED HAND-OFF STILL CHANGES THE ROLE (operator 2026-09-29: "the route still says backender to a frontend task"). `Stay` means the ENGINE
/// stays - crossing engines is priced, and an engine that cannot run here is not an option - but a role is skills and specialists, which swap in
/// place at no cost. `Stay` was read as "nothing changes", so a frontend ask on a pane that had last done backend work kept the Backender label,
/// skills and merge for ever whenever the record could not tell the two apart. Now the role still moves and the sentence says the engine did not.
/// Pure - tested.
pub fn keep_role_on_stay(change: crate::roles::Change, route_role: &str, route_name: &str, current_role: &str) -> crate::roles::Change {
    match change {
        crate::roles::Change::Stay { why } if !route_role.is_empty() && route_role != current_role => crate::roles::Change::Adapter {
            role: route_role.to_string(), why: format!("{why}; the role still changes to {route_name}, the engine does not") },
        other => other,
    }
}

/// A HAND-OFF NEVER LANDS ON A SEAT THAT CANNOT RUN HERE (operator 2026-09-27: "i send a message and nothing
/// happens"). The roles record said "Backender on opencode" from runs measured while Zen was on; with Zen off the
/// hand-off went through anyway and every message on main died 73 s later with `Insufficient account funds` — an
/// error the thread showed as nothing at all. `reachable(engine)` is the caller's test (a chat engine, installed,
/// signed in, its provider switched on); a hand-off to anything else becomes a `Stay` that says why. Pure — tested.
pub fn hold_unreachable_handoff(change: crate::roles::Change, name: &str, reachable: impl Fn(&str) -> bool) -> crate::roles::Change {
    match change {
        crate::roles::Change::Handoff { engine, .. } if !reachable(&engine) => crate::roles::Change::Stay {
            why: format!("staying — {name} would run on {engine}, which cannot take a call here right now (switched off in Settings › Seats, not installed, or not signed in)") },
        other => other,
    }
}

/// WHERE A PANE GOES WHEN ITS OWN SEAT IS OFF — `None` when it may stay. Main had already been handed to OpenCode
/// when Zen was switched off; a pane does not re-decide its engine by itself, so it stayed there and answered
/// every ask with a 402. The Heimdall seat first, when it is a chat engine that can run; else the first chat
/// engine in the router's order that can. Pure — tested.
pub fn seat_to_leave_for(current: crate::cli::Engine, blocked: impl Fn(crate::cli::Engine) -> bool, usable: impl Fn(crate::cli::Engine) -> bool,
                         chosen: Option<crate::cli::Engine>) -> Option<crate::cli::Engine> {
    if !blocked(current) { return None }
    let can = |e: crate::cli::Engine| e != current && CHAT_ENGINES.contains(&e) && usable(e) && !blocked(e);
    chosen.filter(|e| can(*e)).or_else(|| crate::cli::Engine::all().into_iter().find(|e| can(*e)))
}

/// What a pane on `current` does about its seat: stay (the seat is fine), move to another seat, or be STUCK (the seat is off and nothing else can take it). Pure - tested.
#[derive(Debug, PartialEq, Eq)]
pub enum Leave { Stay, To(crate::cli::Engine), Stuck }
pub fn leave_decision(current: crate::cli::Engine, blocked: impl Fn(crate::cli::Engine) -> bool, usable: impl Fn(crate::cli::Engine) -> bool, chosen: Option<crate::cli::Engine>) -> Leave {
    if !blocked(current) { return Leave::Stay }
    match seat_to_leave_for(current, &blocked, usable, chosen) { Some(e) => Leave::To(e), None => Leave::Stuck }
}

/// …applied on the way into `agent::start`: when the pane's engine is switched off, the pane rotates onto the seat
/// `seat_to_leave_for` names — a note in the thread says so — and the rotate's own start is THE start (so the
/// caller returns this instead of starting a second time). `None` when the pane stays where it is; `Some(Err)` when it must leave and no seat can take it.
///
/// Boxed, not `async fn`: start → here → rotate → start is a cycle, and one edge of an async cycle has to be a
/// named, boxed future or the compiler can neither size it nor prove it `Send`.
pub fn leave_blocked_seat<'a>(state: &'a Arc<AppState>, reg: &'a Arc<AgentRegistry>, pane: &'a Value)
    -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Result<Value, String>>> + Send + 'a>> {
    Box::pin(async move {
        let cur = crate::cli::engine_of(&json!(s(pane.get("pane_kind"))))?;
        // A SEAT THAT CANNOT START IS LEFT, NOT ONLY ONE THAT IS SWITCHED OFF (audit 2026-10-05): a seat that is not installed or has no sign-in was started anyway and failed on every message. Quota stays
        // with the pane - the seat's own words about its window are the answer there, and a rotate rewrites the thread's engine.
        let cannot = |e: crate::cli::Engine| crate::cli::model_blocked(e, None).or_else(|| if crate::cli::engine_usable(e) { None } else { Some(format!("{} is not installed or not signed in", e.key())) });
        let why = cannot(cur)?;
        let chosen = crate::cli::engine_of(&json!(crate::cli::heimdall_provider()));
        let pane_id = num(pane.get("id"));
        let to = match leave_decision(cur, |e| cannot(e).is_some(), crate::cli::engine_usable, chosen) {
            Leave::To(to) => to,
            Leave::Stay => return None,
            Leave::Stuck => {
                let sorry = format!("{} cannot take this chat and no other seat can either — {why}", cur.key());
                reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "bad", "text": sorry }), true);
                // …AND THE SEAT IS NOT STARTED ANYWAY (2026-10-01, the Claude-off audit): this returned `None` - "the pane may stay" - right after saying it could not, so with every seat off the
                // session started on the switched-off one and answered. The refusal is the sentence the thread just printed.
                return Some(Err(sorry));
            }
        };
        reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "warn", "text": format!("{} cannot take this chat — it moved to {}. {why}", cur.key(), to.key()) }), true);
        Some(rotate(state, reg, pane, &format!("{} unavailable — moved to {}", cur.key(), to.key()), Some(to.key()), false).await)
    })
}

/// THE NUMBER ON THE MARKER IS THE NUMBER ON THE SIDEBAR (operator 2026-09-27: "episode 2 is saying NOW but is
/// not … The current episode i guess is 3? but is not on the list"). `episodes_of` never shows an empty chapter —
/// a hand-off that lands right after a rotate closes nothing — but the marker counted it, so the thread said
/// "episode 3" while the sidebar, reading the same log, said #2. The marker now counts what the reader will keep.
pub fn next_episode_n(eps: &[Episode]) -> usize { eps.iter().filter(|e| e.turns > 0 || e.replies > 0).count() + 1 }
async fn rotate(state: &Arc<AppState>, reg: &Arc<AgentRegistry>, pane: &Value, reason: &str, engine: Option<&str>, fresh: bool) -> Result<Value, String> {
    // THE HAND-OFF TAX, MEASURED WHERE IT IS ACTUALLY PAID (PLAN-THE-CHAMELEON C4).
    //
    // No public benchmark measures what a provider switch costs, and a synthetic one would measure a synthetic
    // switch. This is the real thing: the clock starts before the session is torn down and stops when the next one
    // is ready, and the transcript that rides in front of the next message is counted as it is built.
    //
    // It is the number that decides whether mid-task switching (C2) is affordable, and until it exists that
    // question can only be argued rather than answered.
    let tax_t0 = crate::cli::now_ms();
    let pane_id = num(pane.get("id"));
    let cwd = s(pane.get("worktree_path"));
    let from_engine = s(pane.get("pane_kind"));
    let to_engine = engine.map(String::from).unwrap_or_else(|| from_engine.clone());
    let text = events_text(&cwd, pane_id);
    let eps = episodes_of(text.lines(), &from_engine);
    let last_seq = eps.last().map(|e| e.seq).unwrap_or(0);
    let switching = to_engine != from_engine;
    let restart = switching || fresh;
    let snapshot = if restart {
        // the next engine session (the other provider, or this one in a fresh window) gets the whole conversation so far, bounded — the handoff
        crate::agents_live::transcript_from_lines(text.lines(), HANDOFF_CAP)
    } else { snapshot_of(text.lines(), last_seq) };
    // what the next message will carry that it would not have carried had nothing moved
    let tax_tokens = if restart { crate::estimate::tokens_of(&snapshot) } else { 0 };
    // A NEW EPISODE NEVER CHANGES THE MODEL (operator 2026-09-23): an in-place restart (a size boundary) comes back on
    // the model the session was on; only a provider switch, the operator's own act, changes the engine
    let keep_model = if restart && !switching { reg.live.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).map(|l| l.model.clone()).filter(|m| !m.is_empty()) } else { None };
    if restart { reg.stop(pane_id); }
    if switching { engine_call(state, "system.cockpit.pane.update", json!({ "pane_id": pane_id, "pane_kind": to_engine }), ctx()).await?; }
    reg.set_root(pane_id, &cwd);
    let closed = eps.last();
    // AN EPISODE BELONGS TO THE BOX IT HAPPENED IN (operator 2026-09-27: "new episodes should be wired to the
    // worktree they belong"). Written when the episode OPENS, because it cannot be recovered afterwards — a merged
    // fork's worktree is gone by the time anyone asks where its work was done. `box` is the worktree path; `agent`
    // is the slug that owns it, which survives the path.
    let box_slug = { let st = settings_of(&pane); let a = s(st.get("agent")); if a.is_empty() { s(pane.get("subagent_name")).trim_start_matches("agent-").to_string() } else { a } };
    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "episode", "reason": reason, "engine": to_engine, "from_engine": from_engine, "snapshot": snapshot, "n": next_episode_n(&eps), "fresh": restart,
        "box": cwd.clone(), "agent": box_slug, "forked_from": settings_of(&pane).get("forked_from").cloned().unwrap_or(Value::Null),
        "closed_title": closed.map(|e| e.title.clone()).unwrap_or_default(), "closed_subtitle": closed.map(|e| e.subtitle.clone()).unwrap_or_default(), "closed_turns": closed.map(|e| e.turns).unwrap_or(0) }), true);
    // the closed episode gets its name from the engine, off this path (the marker carries the deterministic one)
    if let Some(c) = closed.filter(|c| c.turns > 0) {
        let material = snapshot_of(text.lines(), c.seq);
        name_episode_later(reg.clone(), pane_id, cwd.clone(), from_engine.clone(), c.n, material);
    }
    // a consolidation on the same provider leaves the engine session alone; a switch starts the other engine
    // fresh; a size boundary starts THIS engine fresh (the handoff rides with the next message either way)
    if !restart { return Ok(json!({ "ok": true, "consolidated": true, "engine": to_engine })) }
    let started = crate::agent::start(state, reg, pane_id, keep_model, None, true).await;
    // THE BILL FOR MOVING (C4). Emitted whether the move was a provider switch or an in-place restart, because
    // both pay it — and `switch` says which, so the two are never averaged into one meaningless figure.
    //
    // `ms` is wall clock from tear-down to the next session being ready; `tokens` is what the next message now
    // carries that it would not have carried had nothing moved. Neither is an estimate of the other: a fast
    // hand-off that re-sends 20k tokens is expensive, and the screen should be able to say so.
    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "handoff",
        "ms": crate::cli::now_ms() - tax_t0, "tokens": tax_tokens,
        "switch": switching, "from_engine": from_engine, "to_engine": to_engine, "reason": reason,
        "ok": started.is_ok() }), true);
    started
}

/// THE NAME THE ENGINE GIVES (operator 2026-09-21: "a low-tier AI title/subtitle would read better in the
/// timeline"): after a marker closes an episode with turns, one low-tier call on the provider that ran it
/// (`chat.episode_name`, the closed episode's snapshot as the material) lands an `episode_named` line on the
/// log; `episodes_of` takes it over the deterministic name. Off the send path; a failed or shapeless call
/// leaves the deterministic name standing.
fn name_episode_later(reg: Arc<AgentRegistry>, pane_id: i64, cwd: String, engine: String, n: usize, material: String) {
    let Some(engine) = crate::cli::engine_of(&json!(engine)) else { return };
    // the episode may have run on a seat that is switched off today (a Claude chat, after the subscription ended): the name is written by the seat that can
    let engine = crate::cli::seat_or_heimdall(engine);
    let Some(goals) = reg.goals.lock().unwrap_or_else(|p| p.into_inner()).clone() else { return };
    if material.trim().is_empty() { return; }
    tokio::spawn(async move {
        let prompt = crate::prompts::render_base("chat.episode_name", &[("transcript".to_string(), json!(material))].into_iter().collect::<serde_json::Map<String, Value>>());
        let opts = crate::cli::RunOpts { max_turns: Some(1), timeout_ms: Some(90_000), tier: Some(crate::cli::Tier::Low), template: Some("chat.episode_name".into()), ..Default::default() };
        let out = (goals.runner())(engine, vec![crate::cli::ChatMessage { role: "user".into(), content: prompt }], cwd, true, opts, None).await;
        let Ok(o) = out else { return };
        let Some((title, subtitle, story)) = parse_episode_name(&o.parsed.content) else { return };
        reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "episode_named", "n": n, "title": title, "subtitle": subtitle, "story": story, "by": "engine" }), true);
    });
}
/// `{"title", "subtitle", "story"}` out of the engine's reply, bounded like the deterministic names (the story to 900 chars). Pure.
pub fn parse_episode_name(text: &str) -> Option<(String, String, String)> {
    let (a, b) = (text.find('{'), text.rfind('}'));
    let slice = match (a, b) { (Some(a), Some(b)) if b > a => &text[a..=b], _ => text };
    let j: Value = serde_json::from_str(slice).ok()?;
    let one = |k: &str, cap: usize| -> String { let v = s(j.get(k)).split_whitespace().collect::<Vec<_>>().join(" "); let v = v.trim_end_matches('.').trim().to_string(); if v.chars().count() > cap { format!("{}…", v.chars().take(cap - 2).collect::<String>().trim_end()) } else { v } };
    let (title, subtitle, story) = (one("title", 60), one("subtitle", 110), one("story", 900));
    if title.is_empty() { return None; }
    Some((title, subtitle, story))
}

/// Before a chat pane's send: an automatic boundary rotates the episode; the last marker's snapshot (when no
/// message followed it yet) and the retriever's picks ride in front of the text. Returns the text for the
/// engine. A non-chat pane gets its text back untouched.
pub async fn before_send(state: &Arc<AppState>, reg: &Arc<AgentRegistry>, goals: &Arc<crate::goals::Goals>, pane_id: i64, text: String) -> (String, Option<Value>) {
    let Some(pane) = pane_row(state, pane_id).await else { return (text, None) };
    if !is_chat(&pane) { return (text, None); }
    let cwd = s(pane.get("worktree_path"));
    let engine = s(pane.get("pane_kind"));
    let now = crate::cli::now_ms();
    let eps = episodes_of(events_text(&cwd, pane_id).lines(), &engine);
    if let Some(reason) = eps.last().and_then(|e| boundary_for(e, now)) {
        let _ = rotate(state, reg, &pane, &reason, None, boundary_restarts(&reason)).await;
    }
    // ── THE CHAMELEON CHANGES FORM (PLAN-THE-CHAMELEON C0, the call site) ──────────────────────────────────────
    //
    // This is the line that makes it real: before the ask is dispatched, the role that should answer it is chosen
    // and — if that means another engine and the record can tell the two apart — the session hands off. It runs
    // AFTER the boundary rotate above so a form change writes the newest episode marker, which the preamble below
    // then carries into the next message.
    //
    // IT OBEYS THE FLAG. `roles.enabled` off means the operator took the wheel (the switcher's takeover), and
    // nothing here second-guesses that. It is also skipped for an empty ask, where there is nothing to read.
    let d = crate::agents_http::Delegation::of_pane(&pane);
    if d.roles && !text.trim().is_empty() {
        let size = if text.chars().count() <= 400 { "s" } else { "l" };
        let route = crate::roles::route_role_in(&text, size, d.role_objective, &cwd);
        let editor = if size == "s" { "editor-s" } else { "editor-l" };
        let settings_now: Value = serde_json::from_str(&s(pane.get("settings_json"))).unwrap_or(json!({}));
        let now_role = settings_now.get("role").and_then(|r| r.get("current")).and_then(Value::as_str).unwrap_or("").to_string();
        let here = crate::roles::role(&now_role).map(|r| r.domain.key()).unwrap_or("any");
        let there = route.get("domain").and_then(Value::as_str).unwrap_or("any");
        let change = change_form(state, reg, &pane, &route,
                                 &now_role,
                                 crate::merges::role_quality(editor, size, here),
                                 crate::merges::role_quality(editor, size, there)).await;
        let change = keep_role_on_stay(change, route.get("role").and_then(Value::as_str).unwrap_or(""), route.get("name").and_then(Value::as_str).unwrap_or(""), &now_role);
        // REMEMBER THE FORM, so the next ask is weighed against what is actually running rather than against
        // whatever it was when the pane was made.
        if let crate::roles::Change::Adapter { role, .. } | crate::roles::Change::Handoff { role, .. } = &change {
            let mut settings = settings_now.clone();
            // THE ADAPTER ACTUALLY SWAPS (operator 2026-09-28: "yes swap, use this to deliver"). The role's skills and specialists
            // become the pane's kit — added to the pane's OWN base kit, captured the first time the form changes, so what the
            // operator configured is never lost and the previous role's additions go when the next role arrives. The settings
            // update re-bakes the brief, which is what puts the skills in the box. The write SCOPE is not swapped: a role's globs
            // say what kind of work it is, not what the pane may touch, and narrowing it mid-session would refuse edits the ask needs.
            settings = swap_adapter(settings, role);
            let _ = engine_call(state, "system.cockpit.pane.update", json!({ "pane_id": pane_id, "settings": settings }), ctx()).await;
        }
        // …and say it out loud either way. A router that changes who is working without a word is worse than one
        // that does not route, and "it stayed, and here is why" is the half nobody ever prints.
        let said = match &change {
            crate::roles::Change::None => None,
            crate::roles::Change::Adapter { why, .. } | crate::roles::Change::Handoff { why, .. } | crate::roles::Change::Stay { why } => Some(why.clone()),
        };
        // …AND IT SAYS WHICH ROLE (operator 2026-09-28: "at this part where is written role . route --> should show
        // the current role that is working. Every task can switch the role, that is the whole idea behind heimdall").
        // The event carried only the sentence, so the header had no key to look up and fell back to the placeholder
        // "role · routed" — the chameleon changed form on every ask and the chip never said into what.
        let now_role = match &change {
            crate::roles::Change::Adapter { role, .. } | crate::roles::Change::Handoff { role, .. } => Some(role.clone()),
            // `Stay` means the role did not change, so the one already running is still the answer
            crate::roles::Change::Stay { .. } | crate::roles::Change::None => Some(now_role.clone()).filter(|r| !r.is_empty()),
        };
        if let Some(why) = said { reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "role", "text": why, "role": now_role }), true); }
    }
    // the last marker, only when nothing was said after it
    let mut pre = String::new();
    let log = events_text(&cwd, pane_id);
    let mut last_marker: Option<Value> = None; let mut said_after = false;
    for line in log.lines() {
        let Ok(w) = serde_json::from_str::<Value>(line) else { continue };
        let ev = w.get("ev").cloned().unwrap_or(Value::Null);
        if s(ev.get("type")) == "antiloki" && s(ev.get("subtype")) == "episode" { last_marker = Some(ev); said_after = false; }
        else if s(ev.get("type")) == "antiloki" && s(ev.get("subtype")) == "user" { said_after = true; }
    }
    // a provider switch or an in-place restart carries the conversation over — a plain consolidation's engine already has it
    if let (Some(m), false) = (last_marker, said_after) { pre.push_str(&preamble(&m)); }
    // THE CHAT REMEMBERS HEIMDALL'S RUNS (front.rs, part 2; operator 2026-09-29: after a report, "go" reached a chat that had never heard of it):
    // the pane's recent runs - what was asked, what was asked back and answered, the report - ride in front of the turn when they changed
    {
        let dg = crate::front::digest(&goals.list(None), pane_id, now);
        if !dg.is_empty() && crate::front::fresh(pane_id, &dg) { pre.push_str(&dg); pre.push_str("\n\n"); }
        // WHAT THE CHAT IS (front.rs, VOICE_RULE): the voice in front of a team that edits - never "I'm read-only, I won't edit" (operator 2026-10-11)
        { let v = crate::front::voice_rule(is_front(&pane)); if !v.is_empty() { pre.push_str(v); pre.push_str("\n\n"); } }
        // THE PROPOSAL RULE (front.rs): how the chat writes down what it suggests, so a later "yes" is read from the log and not guessed
        pre.push_str(crate::front::PROPOSAL_RULE); pre.push_str("\n\n");
        // THE QUEUE, in front of the chat too: what is done, doing and next for this agent (only when it changed since the last turn that carried it)
        let qb = crate::queue::block_for(pane_id, None);
        if !qb.is_empty() && crate::front::fresh(-pane_id, &qb) { pre.push_str(&qb); pre.push_str("\n"); }
    }
    // the retriever's picks — this chat's own log never rides (it is the conversation itself)
    let (md, used) = crate::chat_auto::compose_for(state, goals, &text, 8_000, &[], &[], Some(pane_id)).await;
    let ctx_ev = if used.is_empty() { None } else { pre.push_str(&md); pre.push_str("\n\n"); Some(json!({ "type": "antiloki", "subtype": "context", "used": used, "chars": md.chars().count() })) };
    (if pre.is_empty() { text } else { format!("{pre}{text}") }, ctx_ev)
}

/// The pane's settings with ROLE's kit swapped in: base kit (captured once, under `role.base`) ∪ the role's skills and
/// specialists, plus `role.current` and the role's packs for the brief. Pure — tested.
pub fn swap_adapter(mut settings: Value, role: &str) -> Value {
    let arr = |v: Option<&Value>| v.and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default();
    let base = settings.get("role").and_then(|r| r.get("base")).cloned()
        .unwrap_or_else(|| json!({ "skills": arr(settings.get("skills")), "specialists": arr(settings.get("specialists")) }));
    let Some(r) = crate::roles::role(role) else { settings["role"] = json!({ "current": role, "base": base }); return settings };
    let union = |a: Vec<String>, b: &[&str]| { let mut out = a; for x in b { if !out.iter().any(|y| y == x) { out.push((*x).to_string()); } } out };
    settings["skills"] = json!(union(arr(base.get("skills")), r.skills));
    settings["specialists"] = json!(union(arr(base.get("specialists")), r.specialists));
    settings["role"] = json!({ "current": role, "base": base, "packs": r.packs });
    settings
}

/// WHAT THE HAND-OFFS HAVE COST, read back from the logs that recorded them (PLAN-THE-CHAMELEON C4).
///
/// Two populations, never averaged together, because they are different events: a PROVIDER SWITCH crosses engines
/// and pays a cold start; an IN-PLACE restart is the same engine in a fresh window. Reporting one number for both
/// would hide exactly the difference the chameleon's stay-put rule trades on.
///
/// Pure over the lines it is given — tested.
pub fn handoff_tax(lines: std::str::Lines<'_>) -> Value {
    let (mut sw_n, mut sw_ms, mut sw_tok) = (0i64, 0i64, 0i64);
    let (mut ip_n, mut ip_ms, mut ip_tok) = (0i64, 0i64, 0i64);
    for line in lines {
        let Ok(w) = serde_json::from_str::<Value>(line) else { continue };
        let ev = w.get("ev").unwrap_or(&w);
        if s(ev.get("type")) != "antiloki" || s(ev.get("subtype")) != "handoff" { continue }
        let (ms, tok) = (num(ev.get("ms")), num(ev.get("tokens")));
        if ev.get("switch").and_then(Value::as_bool).unwrap_or(false) { sw_n += 1; sw_ms += ms; sw_tok += tok; }
        else { ip_n += 1; ip_ms += ms; ip_tok += tok; }
    }
    let mean = |t: i64, n: i64| if n > 0 { (t as f64 / n as f64).round() as i64 } else { 0 };
    // A MEAN OF NOTHING IS NOT ZERO. `n: 0` is the answer until a hand-off has actually happened, and the screen
    // must print "not measured" rather than a reassuring 0 ms.
    json!({
        "switch":   { "n": sw_n, "mean_ms": mean(sw_ms, sw_n), "mean_tokens": mean(sw_tok, sw_n) },
        "in_place": { "n": ip_n, "mean_ms": mean(ip_ms, ip_n), "mean_tokens": mean(ip_tok, ip_n) },
        "note": "wall clock from tear-down to the next session being ready, and the transcript the next message now carries. API-equivalent tokens, not a bill."
    })
}

// ── the HTTP surface ──────────────────────────────────────────────────────────────────────────────
/// `POST cockpit/chat/create {engine, title?}` → the pane
pub async fn create(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body);
    let engine = s(b.get("engine"));
    // no engine named (or an unknown one): the first seat that is installed, signed in AND switched on - not a hard-coded Claude, which fails every chat the day that subscription ends
    let engine = if ENGINES.contains(&engine.as_str()) { engine } else { crate::cli::heimdall_engine(None).map(|e| e.key().to_string()).filter(|k| ENGINES.contains(&k.as_str())).unwrap_or_else(|| "claude-code".to_string()) };
    let title = s(b.get("title")).trim().chars().take(80).collect::<String>();
    let r: Result<Value, String> = async {
        let sid = chats_session(&state).await?;
        let key = format!("chat-{}", crate::cli::now_ms());
        let settings = json!({ "chat": true, "min": true, "title": title, "made": crate::cli::now_ms() });
        let (added, _) = engine_call(&state, "system.cockpit.pane.add", json!({ "session_id": sid, "subagent_name": key, "pane_kind": engine, "provision": false, "settings": settings }), ctx()).await?;
        let pane_id = num(added.get("pane").and_then(|p| p.get("id")));
        if pane_id <= 0 { return Err(format!("pane/add: {}", s(added.get("error")))); }
        let dir = chat_dir(&workspace_root(&state), pane_id);
        std::fs::create_dir_all(dir.join(".antiloki")).map_err(|e| e.to_string())?;
        harden(&dir);
        set_worktree(&state, pane_id, &dir.to_string_lossy()).await;
        let pane = pane_row(&state, pane_id).await.unwrap_or(added["pane"].clone());
        Ok(json!({ "ok": true, "pane": pane, "session_id": sid }))
    }.await;
    match r { Ok(v) => (StatusCode::CREATED, Json(v)).into_response(), Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response() }
}
/// `GET cockpit/chats` → every chat pane with its episodes, newest activity first
pub async fn list(state: Arc<AppState>, reg: Arc<AgentRegistry>) -> Response {
    Json(json!({ "ok": true, "chats": chats_of(&state, &reg).await, "engines": ENGINES })).into_response()
}
/// The chats as data, without the HTTP wrapper — `list` renders it, and `days.rs` folds it into the operator's days.
pub async fn chats_of(state: &Arc<AppState>, reg: &Arc<AgentRegistry>) -> Vec<Value> {
    let conn = state.pick();
    let panes = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, "SELECT p.id, p.session_id, p.subagent_name, p.pane_kind, p.worktree_path, p.settings_json, p.status, p.last_activity FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE s.name = ? AND s.status != 'closed' AND p.status != 'closed'", &[json!(SESSION_NAME)]).unwrap_or(json!([]))
    }).await.unwrap_or(json!([]));
    let mut out = vec![];
    let root = workspace_root(state);
    for p in panes.as_array().cloned().unwrap_or_default() {
        if !is_chat(&p) { continue; }
        let id = num(p.get("id")); let mut cwd = s(p.get("worktree_path")); let engine = s(p.get("pane_kind"));
        if is_legacy_chat_dir(&cwd, id) && !root.is_empty() { if let Some(d) = adopt_legacy_chat(state, &root, id).await { cwd = d; } }
        let settings = settings_of(&p);
        let eps = episodes_of(events_text(&cwd, id).lines(), &engine);
        let last = eps.iter().map(|e| e.last_t).max().unwrap_or(settings.get("made").and_then(Value::as_i64).unwrap_or(0));
        let title = { let t = s(settings.get("title")); if !t.trim().is_empty() { t } else { eps.iter().find(|e| !e.first_ask.is_empty()).map(|e| e.first_ask.clone()).unwrap_or_default() } };
        let st = reg.status(id);
        out.push(json!({ "pane_id": id, "session_id": p.get("session_id"), "key": p.get("subagent_name"), "engine": engine, "title": title, "last": last, "made": settings.get("made"), "turns": eps.iter().map(|e| e.turns).sum::<usize>(), "episodes": eps, "live": st.get("live").and_then(Value::as_bool).unwrap_or(false), "busy": st.get("busy").and_then(Value::as_bool).unwrap_or(false) }));
    }
    out.sort_by_key(|c| std::cmp::Reverse(c.get("last").and_then(Value::as_i64).unwrap_or(0)));
    out
}
/// EVERY AGENT IS HANDED OVER THE SAME WAY (2026-09-25). `episode` was generalised from chats to agents on
/// 2026-09-24; this is that same rule, lifted out so the two cannot drift: a chat, or a pane a recipe made.
pub fn is_agent_pane(p: &Value) -> bool { is_chat(p) || !s(settings_of(p).get("agent")).is_empty() }

/// `POST cockpit/chat/switch {pane_id, engine}` — the same chat OR AGENT continues on another provider, carrying the
/// conversation (operator's queue: "hand-off as a verb on agent panes"). The mechanism was already here; only the
/// filter said no.
pub async fn switch(state: Arc<AppState>, reg: Arc<AgentRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let (pane_id, engine) = (num(b.get("pane_id")), s(b.get("engine")));
    if !ENGINES.contains(&engine.as_str()) { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": format!("engine must be one of {}", ENGINES.join(" · ")) }))).into_response(); }
    let Some(pane) = pane_row(&state, pane_id).await.filter(is_agent_pane) else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such chat or agent" }))).into_response() };
    if s(pane.get("pane_kind")) == engine { return (StatusCode::OK, Json(json!({ "ok": true, "unchanged": true }))).into_response(); }
    match rotate(&state, &reg, &pane, &format!("continued on {engine}"), Some(&engine), false).await {
        Ok(v) => (StatusCode::CREATED, Json(json!({ "ok": true, "engine": engine, "started": v }))).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}
/// `POST cockpit/chat/episode {pane_id, reason?}` — New topic: close the episode, open the next
pub async fn episode(state: Arc<AppState>, reg: Arc<AgentRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let pane_id = num(b.get("pane_id"));
    let reason = { let r = s(b.get("reason")); if r.trim().is_empty() { "you marked a new topic".to_string() } else { r } };
    // HOW THE WORK IS DELIVERED (operator 2026-09-24): "merge" lands it in the workspace (the default and what
    // closing an episode has always meant); "patch" writes a patch series and its review document to the outbox,
    // which needs no forge, no token and no account. A hosted forge is the same content over its own REST API.
    let deliver = { let d = s(b.get("deliver")); if d.is_empty() { "merge".to_string() } else { d } };
    // EVERY AGENT HAS EPISODES, not only the chats (2026-09-24). The roster's agents are panes with a `recipe`,
    // not `chat: true`, so the Close episode button under them was posting to a handler that refused them.
    let Some(pane) = pane_row(&state, pane_id).await.filter(is_agent_pane) else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such agent" }))).into_response() };
    let cwd = s(pane.get("worktree_path"));
    let eps = episodes_of(events_text(&cwd, num(pane.get("id"))).lines(), &s(pane.get("pane_kind")));
    if eps.last().map(|e| e.turns).unwrap_or(0) == 0 { return (StatusCode::OK, Json(json!({ "ok": true, "unchanged": true, "why": "the open episode is empty — it is a fresh topic already" }))).into_response(); }
    let closing = eps.len().max(1);
    let title = eps.last().map(|e| if e.title.trim().is_empty() { e.first_ask.clone() } else { e.title.clone() }).unwrap_or_default();
    match rotate(&state, &reg, &pane, &reason, None, false).await {
        Ok(v) => {
            // CLOSING AN EPISODE IS A MERGE (operator 2026-09-24) — for an agent that has a box of its own. One
            // button and one act: the consolidation the operator already knows, and then the code lands. An agent
            // in the shared workspace has nothing to merge, and `merge` comes back null rather than pretending.
            let merge = if deliver == "patch" { Value::Null } else { box_merge(&state, &pane, &cwd, &title, closing).await };
            // AND IT GOES INTO THE REPO'S MEMORY (operator 2026-09-24: "all the memory and episodes should live
            // and be consolidated in a sqlite db on the repo"). The event log is the raw record; this is the
            // consolidation, in the workspace's own memory.sqlite — so an episode outlives the box it ran in.
            let filed = file_episode(&state, &pane, &cwd, closing, &merge).await;
            let delivered = if deliver == "patch" || deliver == "pr" {
                let g = reg.goals.lock().unwrap_or_else(|p| p.into_inner()).clone();
                deliver_work(&state, g, &pane, &cwd, closing, &title, &deliver).await
            } else { Value::Null };
            (StatusCode::CREATED, Json(json!({ "ok": true, "started": v, "merge": merge, "filed": filed, "delivered": delivered }))).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

/// GET /api/v1/cockpit/deliver/preflight?pane_id=N - what "pull request" will do for this agent's repo, before it is clicked (forge::preflight_of).
pub async fn deliver_preflight(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let pane_id = q.get("pane_id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    let Some(pane) = pane_row(&state, pane_id).await.filter(is_agent_pane) else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such agent" }))).into_response() };
    let root = workspace_root_db(&state).await;
    let cwd = s(pane.get("worktree_path"));
    if root.is_empty() { return Json(json!({ "ok": false, "error": "no workspace open" })).into_response() }
    let slug = { let a = s(settings_of(&pane).get("agent")); if a.is_empty() { s(pane.get("subagent_name")).trim_start_matches("agent-").to_string() } else { a } };
    let out = tokio::task::spawn_blocking(move || {
        let cfg = std::fs::read_to_string(Path::new(&root).join(".antiloki").join("forge.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(json!({}));
        let remote = crate::proc::cmd("git").args(["remote", "get-url", "origin"]).current_dir(&root).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        let (_, host, _) = crate::forge::detect(&remote, &cfg);
        let kind = crate::forge::detect(&remote, &cfg).0;
        let token = if host.is_empty() { Err("no remote".to_string()) } else { crate::forge::find_token(kind, &host).map(|t| t.from) };
        let (inst, acct) = crate::forge::gh_state();
        let branch = if slug.is_empty() { String::new() } else { crate::boxes::agent_branch(&slug) };
        let _ = &cwd;
        let mut v = crate::forge::preflight_of(&remote, &cfg, &crate::boxes::base_branch(&root), &branch, inst, acct.as_deref(), token.as_ref().map(|s| s.as_str()).map_err(|e| e.as_str()));
        v["root_name"] = json!(Path::new(&root).file_name().and_then(|n| n.to_str()).unwrap_or("repo"));
        v
    }).await.unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
    let mut out = out;
    // THE ONE-TIME NONCE `deliver/publish` MUST ECHO (audit 2026-10-01, F-01): the click that creates a GitHub repository has to be preceded by THIS read.
    if out.get("ok") == Some(&json!(true)) { out["publish_nonce"] = json!(publish_nonce_issue(pane_id)); out["publish_nonce_ttl_s"] = json!(PUBLISH_NONCE_TTL.as_secs()); }
    Json(out).into_response()
}

/// How long a preflight's nonce stays good: the operator reads the dialog, then clicks.
const PUBLISH_NONCE_TTL: std::time::Duration = std::time::Duration::from_secs(600);
fn publish_nonces() -> &'static std::sync::Mutex<std::collections::HashMap<i64, (String, std::time::Instant)>> {
    static N: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<i64, (String, std::time::Instant)>>> = std::sync::OnceLock::new();
    N.get_or_init(Default::default)
}
/// A fresh nonce for this pane (the previous one, if any, is void). 128 random bits, hex. In memory only - a restarted serve asks for a new preflight.
pub fn publish_nonce_issue(pane_id: i64) -> String {
    use rand::RngCore;
    let mut raw = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let n = hex::encode(raw);
    let mut m = publish_nonces().lock().unwrap_or_else(|p| p.into_inner());
    m.retain(|_, (_, at)| at.elapsed() < PUBLISH_NONCE_TTL);
    m.insert(pane_id, (n.clone(), std::time::Instant::now()));
    n
}
/// Spend the nonce: right pane, right value, not older than the TTL, and ONCE (it is gone whether or not the publish then succeeds). `ttl` is a parameter so the test can age it.
pub fn publish_nonce_redeem(pane_id: i64, given: &str, ttl: std::time::Duration) -> Result<(), &'static str> {
    let mut m = publish_nonces().lock().unwrap_or_else(|p| p.into_inner());
    let Some((n, at)) = m.get(&pane_id).cloned() else { return Err("publishing needs a fresh preflight: call GET /api/v1/cockpit/deliver/preflight?pane_id=N first and send its publish_nonce back as `nonce`") };
    if given.is_empty() || given != n { return Err("the nonce does not match this agent's last preflight - read the preflight again and send its publish_nonce back as `nonce`") }
    m.remove(&pane_id);
    if at.elapsed() >= ttl { return Err("the preflight's nonce expired (10 minutes) - read the preflight again") }
    Ok(())
}

/// POST /api/v1/cockpit/deliver/publish {pane_id, name?, private?, confirm: true, nonce} - CREATE THE ONLINE REPO for a local-only workspace and push it (`gh repo create`). An outward act on the
/// operator's GitHub account, so it needs `confirm: true` from a click that said what it would do AND the `nonce` that `GET deliver/preflight` returned as `publish_nonce` (one use, 10 minutes, per pane); it refuses when a remote already exists, when `gh` is not logged in, or for a bad name.
pub async fn deliver_publish(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body);
    let bad = |m: &str| (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": m }))).into_response();
    if b.get("confirm").and_then(Value::as_bool) != Some(true) { return bad("publishing creates a repository on your GitHub account - it needs an explicit confirm") }
    let pane_id = num(b.get("pane_id"));
    if pane_row(&state, pane_id).await.filter(is_agent_pane).is_none() { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such agent" }))).into_response() }
    // THE PREFLIGHT'S NONCE (audit 2026-10-01, F-01): `{pane_id, confirm:true}` alone was enough for any page that could reach the port to create a repository on the operator's account
    if let Err(m) = publish_nonce_redeem(pane_id, &s(b.get("nonce")), PUBLISH_NONCE_TTL) { return bad(m) }
    let root = workspace_root_db(&state).await;
    if root.is_empty() { return bad("no workspace open") }
    let default = Path::new(&root).file_name().and_then(|n| n.to_str()).unwrap_or("repo").to_string();
    let name = { let n = s(b.get("name")); if n.trim().is_empty() { default } else { n.trim().to_string() } };
    let private = b.get("private").and_then(Value::as_bool).unwrap_or(true);
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let has_remote = crate::proc::cmd("git").args(["remote", "get-url", "origin"]).current_dir(&root).output().ok().map_or(false, |o| o.status.success());
        if has_remote { return Err("this repo already has an origin remote - nothing to publish".into()) }
        let (_, acct) = crate::forge::gh_state();
        let acct = acct.ok_or("the GitHub CLI is not logged in: run `gh auth login` in your terminal first")?;
        let url = crate::forge::publish_repo(&root, &name, private)?;
        Ok(json!({ "ok": true, "url": url, "name": name, "private": private, "account": acct }))
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match out { Ok(v) => (StatusCode::CREATED, Json(v)).into_response(), Err(e) => bad(&e) }
}

/// DELIVER THE EPISODE AS A PATCH (operator 2026-09-24: "not dependency on github should be agnostic"). The
/// episode is sealed on its own branch first — the same seal the merge path uses — and then written out as a
/// series plus the review document the run's own ledger produces. Nothing leaves the machine.
async fn deliver_work(state: &Arc<AppState>, goals: Option<Arc<crate::goals::Goals>>, pane: &Value, cwd: &str, n: usize, title: &str, how: &str) -> Value {
    let root = workspace_root_db(state).await;
    if root.is_empty() || cwd.is_empty() || cwd == root {
        return json!({ "ok": false, "error": "this agent works in the workspace itself — there is no branch to turn into a patch. Give it a box of its own." });
    }
    let settings = settings_of(pane);
    let slug = { let a = s(settings.get("agent")); if a.is_empty() { s(pane.get("subagent_name")).trim_start_matches("agent-").to_string() } else { a } };
    // the story is what makes the patch worth reading, so it is assembled from the runs this agent actually made
    let story = goals.as_ref().and_then(|g| {
        let runs: Vec<Value> = g.list(None).into_iter().map(|r| json!(r)).collect();
        let mine: Vec<&Value> = runs.iter().filter(|r| { let p = s(r.get("parent")); p == slug || p == format!("agent-{slug}") }).collect();
        let newest = mine.into_iter().max_by_key(|r| num(r.get("ended")).max(num(r.get("created"))))?;
        Some(crate::work_story::story_with(newest, &[], &runs, crate::cli::now_ms()))
    }).unwrap_or(json!({ "goal": title, "stages": [], "gates": [], "who": {}, "summary": {} }));
    let (cwd2, slug2, title2, root2) = (cwd.to_string(), slug.clone(), title.to_string(), root.clone());
    let scope2 = pane_scope_globs(pane);
    let how = how.to_string();
    let prep = tokio::task::spawn_blocking(move || {
        let bx = std::path::PathBuf::from(&cwd2);
        let sealed = match crate::boxes::seal_episode(&bx, &slug2, n, &title2) { Ok(v) => v, Err(e) => return Err(json!({ "ok": false, "error": e })) };
        let branch = crate::boxes::episode_branch(&slug2, n);
        let base = crate::boxes::agent_branch(&slug2);
        // THE SEAL KNOWS WHAT CHANGED even when no goal run does. A turn the router kept inline leaves no run, so
        // the story falls back to a bare one — and a review document that cannot name the files it is about is
        // not worth writing. The commit's own file list is real evidence; it goes in.
        let mut story = story;
        let files = sealed.get("files").cloned().unwrap_or(json!([]));
        if story.pointer("/summary/files/written").map(|f| f.as_array().map(Vec::is_empty).unwrap_or(true)).unwrap_or(true) {
            if !story.get("summary").map(Value::is_object).unwrap_or(false) { story["summary"] = json!({}); }
            story["summary"]["files"] = json!({ "written": files, "read": 0 });
        }
        // the same fence as the merge: a pull request or a patch is also "the work landing"
        { let fl: Vec<String> = files.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
          if let Some(mut b) = scope_block(&scope2, &fl) { b["sealed"] = sealed.clone(); return Err(b) } }
        let body = crate::forge::pr_body(&story, &title2, &branch);
        // a PULL REQUEST needs the workspace's remote, and the base it targets is the branch the workspace is on
        let (kind, host, repo) = if how == "pr" {
            let cfg = std::fs::read_to_string(std::path::Path::new(&root2).join(".antiloki").join("forge.json")).ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(json!({}));
            let remote = crate::proc::cmd("git").args(["remote", "get-url", "origin"]).current_dir(&root2).output().ok()
                .filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
            crate::forge::detect(&remote, &cfg)
        } else { (crate::forge::Kind::Patch, String::new(), String::new()) };
        let base = if how == "pr" { crate::boxes::base_branch(&root2) } else { base };
        let req = crate::forge::PrRequest { root: cwd2.clone(), branch, base, title: title2, body };
        if kind == crate::forge::Kind::Patch {
            // …which is also where a `pr` lands when the remote is not a forge we can talk to: the work is
            // delivered either way, and the reason is said rather than the request simply failing
            let why = if how == "pr" { Some("this workspace's remote is not a forge antiloki can file against — written as a patch instead. Name the host in .antiloki/forge.json if it is a self-hosted GitLab or Gitea.") } else { None };
            return match <crate::forge::PatchForge as crate::forge::Forge>::open_pr(&crate::forge::PatchForge::at(&root2), &req) {
                Ok(r) => Ok((json!({ "ok": true, "kind": r.kind, "path": r.local.map(|p| p.to_string_lossy().to_string()), "note": r.note, "sealed": sealed, "why": why }), None)),
                Err(e) => Err(json!({ "ok": false, "error": e, "sealed": sealed })),
            };
        }
        Ok((json!({ "sealed": sealed }), Some((kind, host, repo, req))))
    }).await.unwrap_or_else(|e| Err(json!({ "ok": false, "error": e.to_string() })));

    let (mut out, hosted) = match prep { Ok(v) => v, Err(e) => return e };
    let Some((kind, host, repo, req)) = hosted else { return out };

    // the hosted half is async: a push, a pull request, then one commit status per gate
    let token = match crate::forge::find_token(kind, &host) { Ok(t) => t, Err(e) => {
        out["ok"] = json!(false); out["error"] = json!(e); out["kind"] = json!(kind.key()); return out } };
    if kind == crate::forge::Kind::GitLab { out["ok"] = json!(false); out["error"] = json!("antiloki cannot open merge requests on GitLab yet: the branch can be pushed, or taken as a patch"); out["kind"] = json!(kind.key()); return out }
    let gh = crate::forge::GitHubForge { kind, host: host.clone(), repo: repo.clone(), token };
    match gh.open_pr_async(&req).await {
        Ok(r) => {
            out["ok"] = json!(true); out["kind"] = json!(r.kind); out["url"] = json!(r.url); out["note"] = json!(r.note);
            out["token_from"] = json!(gh.token.from);
            // THE GATES BECOME THE FORGE'S OWN CHECKS. A failing one then blocks the merge through GitHub's
            // branch protection rather than through ours — which is the difference between asking to be trusted
            // and reporting into the system that already is.
            let gates = last_gates(&req.root);
            if !gates.is_empty() {
                match gh.post_gates(&req.root, &req.branch, &gates, "").await {
                    Ok(n) => out["statuses"] = json!(n),
                    Err(e) => out["statuses_error"] = json!(e),
                }
            }
            out
        }
        Err(e) => { out["ok"] = json!(false); out["kind"] = json!(kind.key()); out["error"] = json!(e); out }
    }
}

/// The gate results this box last recorded, for posting as statuses. Empty when the gates never ran.
fn last_gates(root: &str) -> Vec<Value> {
    std::fs::read_to_string(std::path::Path::new(root).join(".antiloki").join("gates.json")).ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_array().cloned()).unwrap_or_default()
}
/// Write the episode that just closed into `<workspace>/.antiloki/db/memory.sqlite` as a consolidation. Re-read
/// from the log AFTER the rotation, because that is when the closing marker carries the title, the subtitle and
/// the story the engine wrote for it — reading before the rotation gets an episode that has not been named yet.
async fn file_episode(state: &Arc<AppState>, pane: &Value, cwd: &str, n: usize, merge: &Value) -> Value {
    let root = workspace_root_db(state).await;
    if root.is_empty() { return Value::Null }
    let pane_id = num(pane.get("id"));
    let engine = s(pane.get("pane_kind"));
    let settings = settings_of(pane);
    let slug = { let a = s(settings.get("agent")); if a.is_empty() { s(pane.get("subagent_name")).trim_start_matches("agent-").to_string() } else { a } };
    let eps = episodes_of(events_text(cwd, pane_id).lines(), &engine);
    let Some(e) = eps.iter().find(|e| e.n == n) else { return Value::Null };
    let closed = crate::episodes_db::Closed {
        agent_slug: slug, pane_id, n, engine,
        title: e.title.clone(), subtitle: e.subtitle.clone(),
        snapshot: e.snapshot.clone(), story: e.story.clone(),
        first_t: e.t, last_t: e.last_t, turns: e.turns,
        branch: s(merge.get("sealed").and_then(|x| x.get("branch"))),
        merged_into: s(merge.get("merge").and_then(|x| x.get("workspace"))),
    };
    let root2 = root.clone();
    match tokio::task::spawn_blocking(move || crate::episodes_db::record(&root2, &closed)).await {
        Ok(Ok(id)) => json!({ "ok": true, "id": id, "db": crate::episodes_db::db_path(&root).to_string_lossy() }),
        Ok(Err(e)) => json!({ "ok": false, "error": e }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}
async fn workspace_root_db(state: &Arc<AppState>) -> String {
    let conn = state.pick();
    tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::engine_workspace_root::getWorkspaceRoot(&c).as_str().unwrap_or("").to_string()
    }).await.unwrap_or_default()
}

/// Seal and merge the episode that just closed, then cut the next one's branch. `Null` when this agent shares the
/// workspace — there is no box, so there is nothing to land.
/// THE PANE'S EDIT SCOPE, as globs (an empty list = unrestricted). Pure - tested.
pub fn pane_scope_globs(pane: &Value) -> Vec<String> {
    let raw = pane.get("scope_json");
    let v: Value = match raw { Some(Value::String(t)) => serde_json::from_str(t).unwrap_or(Value::Null), Some(v) => v.clone(), None => Value::Null };
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).filter(|g| !g.trim().is_empty()).collect()).unwrap_or_default()
}
/// THE WHITELIST IS ENFORCED WHERE THE WORK LANDS (operator 2026-09-30: "scope is being respected on the agent?"). Every agent's brief says edits outside its scope "WILL BE BLOCKED at integration",
/// and nothing did: the pre-write hook guards Claude panes only, the sentry only offers an undo afterwards, and a Cursor or Codex agent could change anything and have it merged. Before an episode
/// is merged or delivered, the files it changed are held against the scope: any outside it stops the merge (the work stays safe on its branch) and are named. `None` = all inside, or no scope.
/// antiloki's own bookkeeping (`.antiloki/`) is never the agent's. Pure - tested.
pub fn scope_block(scope: &[String], files: &[String]) -> Option<Value> {
    if scope.is_empty() { return None }
    let outside: Vec<String> = files.iter().filter(|f| !f.starts_with(".antiloki/") && !antiloki_rs::agents_in_pane_scope_sentry::covered(f, scope)).cloned().collect();
    if outside.is_empty() { return None }
    let shown = outside.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
    Some(json!({ "ok": false, "blocked": "scope", "outside": outside, "scope": scope,
        "error": format!("{} file(s) changed outside this agent's scope ({}): {}{} - nothing was merged, the work is safe on its branch. Undo those changes (the out-of-scope badge offers it) or widen the agent's scope, then close the episode again.",
            outside.len(), scope.join(", "), shown, if outside.len() > 8 { ", …" } else { "" }) }))
}
async fn box_merge(state: &Arc<AppState>, pane: &Value, cwd: &str, title: &str, closing: usize) -> Value {
    if cwd.is_empty() { return Value::Null }
    let root = workspace_root_db(state).await;
    if root.is_empty() || cwd == root { return Value::Null }
    let settings = settings_of(pane);
    let slug = { let a = s(settings.get("agent")); if a.is_empty() { s(pane.get("subagent_name")).trim_start_matches("agent-").to_string() } else { a } };
    let (cwd, title, slug2) = (cwd.to_string(), title.to_string(), slug.clone());
    let scope = pane_scope_globs(pane);
    tokio::task::spawn_blocking(move || {
        let bx = std::path::PathBuf::from(&cwd);
        let sealed = match crate::boxes::seal_episode(&bx, &slug2, closing, &title) { Ok(v) => v, Err(e) => return json!({ "ok": false, "error": e }) };
        let files: Vec<String> = sealed.get("files").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
        if let Some(mut b) = scope_block(&scope, &files) { b["sealed"] = sealed; return b }
        let merged = match crate::boxes::merge_episode(&root, &bx, &slug2, closing, true) { Ok(v) => v, Err(e) => return json!({ "ok": false, "sealed": sealed, "error": e }) };
        // the next episode gets its own branch, whatever the merge said — the conversation carries on either way
        let next = crate::boxes::start_episode(&bx, &slug2, closing + 1).ok();
        json!({ "ok": merged.get("ok").cloned().unwrap_or(json!(true)), "sealed": sealed, "merge": merged, "next_branch": next })
    }).await.unwrap_or(Value::Null)
}
/// `POST cockpit/chat/rename {pane_id, title}`
pub async fn rename(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body);
    let (pane_id, title) = (num(b.get("pane_id")), s(b.get("title")).trim().chars().take(80).collect::<String>());
    let Some(pane) = pane_row(&state, pane_id).await.filter(is_chat) else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such chat" }))).into_response() };
    let mut settings = settings_of(&pane); settings["title"] = json!(title);
    match engine_call(&state, "system.cockpit.pane.update", json!({ "pane_id": pane_id, "settings": settings }), ctx()).await {
        Ok(_) => Json(json!({ "ok": true, "title": title })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}
/// `POST cockpit/chat/close {pane_id}` — the chat leaves the list (its log stays on disk)
pub async fn close(state: Arc<AppState>, reg: Arc<AgentRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let pane_id = num(b.get("pane_id"));
    let Some(_) = pane_row(&state, pane_id).await.filter(is_chat) else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such chat" }))).into_response() };
    reg.stop(pane_id);
    let conn = state.pick();
    // through the action that owns the table, like every other write to it (audit 2026-09-28)
    let ok = engine_call(&state, "system.cockpit.pane.update", json!({ "pane_id": pane_id, "status": "closed" }), ctx()).await.is_ok();
    Json(json!({ "ok": ok })).into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_role_change_swaps_the_kit_and_keeps_the_operators_own() {
        let s0 = serde_json::json!({ "skills": ["my-skill"], "specialists": [], "title": "Main" });
        let fe = super::swap_adapter(s0, "frontender");
        let skills: Vec<String> = serde_json::from_value(fe["skills"].clone()).unwrap();
        assert!(skills.contains(&"my-skill".to_string()) && skills.contains(&"antiloki-file-review".to_string()), "{skills:?}");
        assert_eq!(fe["role"]["current"], "frontender"); assert_eq!(fe["title"], "Main");
        // the next role replaces the previous role's kit, never the operator's
        let be = super::swap_adapter(fe, "backend-database");
        let skills: Vec<String> = serde_json::from_value(be["skills"].clone()).unwrap();
        assert!(skills.contains(&"my-skill".to_string()) && skills.contains(&"antiloki-blast-radius".to_string()));
        assert!(!skills.contains(&"antiloki-file-review".to_string()), "the frontender's kit left with it: {skills:?}");
    }

    /// READ-ONLY IS ITS OWN FLAG (operator 2026-09-27: "first is not suppose to be read-only, we dont have this
    /// anymore"). `chat` still selects antiChat's chrome; whether the pane may write is `readonly`, and only a
    /// plain chat that never said falls back to the old rule.
    #[test]
    fn a_chat_that_says_it_can_write_can_and_one_that_says_nothing_still_cannot() {
        let pane = |s: &str| serde_json::json!({ "settings_json": s });
        // FRONT MODE (2026-09-29): a chat is the voice and does not write, whatever `readonly` says; the old say comes back with `front.enabled: false`
        assert!(super::is_readonly(&pane(r#"{"chat":true,"readonly":false}"#)), "front mode: a chat never writes; Heimdall does");
        assert!(!super::is_readonly(&pane(r#"{"chat":true,"readonly":false,"front":{"enabled":false}}"#)), "main and its forks: the chrome without the shackle, when front mode is off");
        assert!(super::is_front(&pane(r#"{"chat":true}"#)) && !super::is_front(&pane(r#"{"chat":true,"front":{"enabled":false}}"#)) && !super::is_front(&pane(r#"{"agent":"patcher"}"#)), "only a chat has a front");
        assert!(super::is_readonly(&pane(r#"{"chat":true}"#)), "a plain chat (chat_pane::open) reads only, as it always did");
        assert!(super::is_readonly(&pane(r#"{"chat":false,"readonly":true}"#)), "an explicit yes is a yes, chrome or not");
        assert!(!super::is_readonly(&pane(r#"{"agent":"patcher"}"#)), "an ordinary agent pane writes");
        assert!(super::is_chat(&pane(r#"{"chat":true,"readonly":false}"#)), "…and it is still a chat for the chrome and the episodes list");
    }

    #[test]
    fn a_marker_after_an_empty_chapter_takes_the_number_the_sidebar_will_show() {
        let log = vec![
            r#"{"seq":1,"t":10,"ev":{"type":"antiloki","subtype":"user","text":"first"}}"#,
            r#"{"seq":2,"t":11,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}}"#,
            r#"{"seq":3,"t":12,"ev":{"type":"antiloki","subtype":"episode","reason":"new topic","n":2}}"#,
        ].join("\n");
        let eps = episodes_of(log.lines(), "claude-code");
        assert_eq!(eps.len(), 2, "the closed chapter and the open, empty one");
        assert_eq!(super::next_episode_n(&eps), 2, "a hand-off now closes nothing: the open chapter keeps #2, it does not become #3");
        let log2 = format!("{log}\n{}", r#"{"seq":4,"t":13,"ev":{"type":"antiloki","subtype":"user","text":"said something"}}"#);
        assert_eq!(super::next_episode_n(&episodes_of(log2.lines(), "claude-code")), 3, "once something was said in it, the next chapter is #3");
    }

    /// AN EPISODE REMEMBERS WHERE IT HAPPENED (operator 2026-09-27: "new episodes should be wired to the worktree
    /// they belong … only on merge with the master they merge with master episodes (but showing wich episodes came
    /// from outside)"). Written at the marker, because it cannot be recovered later: a merged fork's worktree is
    /// gone by the time anyone asks where its work was done.
    #[test]
    fn an_episode_records_the_box_it_happened_in_and_says_when_it_came_from_a_fork() {
        let log = vec![
            r#"{"seq":1,"t":10,"ev":{"type":"antiloki","subtype":"user","text":"on master"}}"#,
            r#"{"seq":2,"t":11,"ev":{"type":"antiloki","subtype":"episode","reason":"new topic","box":"/w/main","agent":"main"}}"#,
            r#"{"seq":3,"t":12,"ev":{"type":"antiloki","subtype":"user","text":"in the fork"}}"#,
            r#"{"seq":4,"t":13,"ev":{"type":"antiloki","subtype":"episode","reason":"new topic","box":"/w/sly-twin","agent":"main-sly-twin","forked_from":25}}"#,
        ].join("\n");
        let eps = episodes_of(log.lines(), "claude-code");
        assert_eq!(eps.len(), 3);

        // EPISODE ONE PREDATES ITS OWN MARKER — it begins at the first line, so there is no box to read. Empty is
        // the honest answer; filling it from the pane's CURRENT box would claim work happened where it did not.
        assert_eq!(eps[0].r#box, "", "the first episode has no marker to carry a box");
        assert_eq!(eps[0].forked_from, None);

        assert_eq!(eps[1].r#box, "/w/main");
        assert_eq!(eps[1].agent, "main");
        assert_eq!(eps[1].forked_from, None, "master's own work is not from outside");

        // …and the one opened in a fork says so, with the pane it came from
        assert_eq!(eps[2].r#box, "/w/sly-twin");
        assert_eq!(eps[2].agent, "main-sly-twin");
        assert_eq!(eps[2].forked_from, Some(25), "this is what marks a merged episode as from outside");
    }

    /// THE PLANNER KNOWS THE CONVERSATION (operator 2026-09-29): earlier episodes consolidated, this one in full, the middle dropped only under pressure.
    #[test]
    fn the_brief_carries_the_earlier_episodes_consolidated_and_this_one_in_full() {
        let log = vec![
            r#"{"seq":1,"t":10,"ev":{"type":"antiloki","subtype":"user","text":"add a dark theme toggle"}}"#.to_string(),
            r#"{"seq":2,"t":11,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"Added it to the header."}]}}}"#.to_string(),
            r#"{"seq":3,"t":12,"ev":{"type":"antiloki","subtype":"episode","reason":"New topic","snapshot":"operator: add a dark theme toggle | assistant: Added it to the header.","engine":"claude-code","closed_title":"Dark theme","closed_subtitle":"toggle in the header"}}"#.to_string(),
            r#"{"seq":4,"t":13,"ev":{"type":"antiloki","subtype":"episode_named","n":1,"title":"Dark theme","subtitle":"toggle in the header","story":"The operator asked for a dark theme; a toggle now lives in the header and persists."}}"#.to_string(),
            r#"{"seq":5,"t":14,"ev":{"type":"antiloki","subtype":"user","text":"what do you suggest next?"}}"#.to_string(),
            r#"{"seq":6,"t":15,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"Persist the cart, then filtering."}]}}}"#.to_string(),
            r#"{"seq":7,"t":16,"ev":{"type":"antiloki","subtype":"user","text":"go with the cart"}}"#.to_string(),
        ].join("\n");
        let b = brief_of(log.lines(), "claude-code", 6000);
        assert!(b.contains("EARLIER EPISODES") && b.contains("Dark theme") && b.contains("a toggle now lives in the header and persists"), "{b}");
        assert!(b.contains("THIS EPISODE (#2), in full") && b.contains("operator: what do you suggest next?") && b.contains("assistant: Persist the cart, then filtering.") && b.contains("operator: go with the cart"), "{b}");
        assert!(!b.contains("add a dark theme toggle\nassistant"), "the earlier episode is consolidated, not replayed turn by turn: {b}");
        // pressure: many turns, a small budget - the opening and the latest survive, the middle goes and says so
        let mut many = vec![r#"{"seq":1,"t":1,"ev":{"type":"antiloki","subtype":"user","text":"OPENING ask about the store"}}"#.to_string()];
        for i in 2..60 { many.push(format!(r#"{{"seq":{i},"t":{i},"ev":{{"type":"antiloki","subtype":"user","text":"middle turn number {i} with some words to take room"}}}}"#)); }
        many.push(r#"{"seq":99,"t":99,"ev":{"type":"antiloki","subtype":"user","text":"LATEST: do that one"}}"#.to_string());
        let small = brief_of(many.join("\n").lines(), "claude-code", 1500);
        assert!(small.contains("OPENING ask") && small.contains("LATEST: do that one") && small.contains("in the middle not shown"), "{small}");
        assert!(small.len() < 2200, "the budget holds: {}", small.len());
        // no history, no brief
        assert!(brief_of("".lines(), "claude-code", 6000).is_empty());
        assert!(conversation_brief(".", 0, "claude-code", 6000).is_empty());
    }

    /// ONE EPISODE'S TURNS ARE BOUNDED AT BOTH ENDS (operator 2026-09-27: "the episodes are clickable they carry the
    /// history of the conversation"). `snapshot_of` trims and keeps only the tail, which is right for a prompt and
    /// wrong for reading: clicking an episode must give every turn it contained and none of the next episode's.
    #[test]
    fn an_episodes_turns_are_the_log_between_its_seq_and_the_next_ones() {
        let log = vec![
            r#"{"seq":1,"t":10,"ev":{"type":"antiloki","subtype":"user","text":"first ask"}}"#,
            r#"{"seq":2,"t":11,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"first reply"}]}}}"#,
            r#"{"seq":3,"t":12,"ev":{"type":"antiloki","subtype":"episode","snapshot":"s"}}"#,
            r#"{"seq":4,"t":13,"ev":{"type":"antiloki","subtype":"user","text":"second ask"}}"#,
            r#"{"seq":5,"t":14,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"second reply"}]}}}"#,
        ].join("\n");

        // episode 1: bounded above by episode 2's marker — the second ask must NOT leak in
        let one = turns_between(log.lines(), 1, Some(3));
        assert_eq!(one.len(), 2, "two turns in episode one: {one:?}");
        assert_eq!(one[0]["role"], "you"); assert_eq!(one[0]["text"], "first ask");
        assert_eq!(one[1]["role"], "antiloki"); assert_eq!(one[1]["text"], "first reply");

        // episode 2 is the live one: no upper bound, and it must not carry episode one's turns back
        let two = turns_between(log.lines(), 4, None);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0]["text"], "second ask");
        assert_eq!(two[1]["text"], "second reply");

        // UNTRIMMED, unlike the snapshot: this is the conversation to read, not a summary of it
        let long = format!(r#"{{"seq":1,"t":1,"ev":{{"type":"antiloki","subtype":"user","text":"{}"}}}}"#, "x".repeat(900));
        let full = turns_between(long.lines(), 1, None);
        assert_eq!(full[0]["text"].as_str().unwrap().chars().count(), 900, "the reader gets the whole turn");

        // and the boundaries line up with the cutter itself, so the two can never disagree
        let eps = episodes_of(log.lines(), "claude-code");
        assert_eq!(eps.len(), 2);
        assert_eq!(turns_between(log.lines(), eps[0].seq, Some(eps[1].seq)).len(), 2);

        // A DUPLICATED LOG IS ONE CONVERSATION, NOT TWO (2026-09-27). pane 25's file held every event twice — 30
        // lines, 16 distinct seqs — so the reader rendered the conversation twice and the counts doubled. Both
        // readers de-dupe on seq, which is the field that identifies an event.
        let doubled = format!("{log}\n{log}");
        let once = turns_between(doubled.lines(), 1, Some(3));
        assert_eq!(once.len(), 2, "the same two turns, not four: {once:?}");
        let deps = episodes_of(doubled.lines(), "claude-code");
        assert_eq!(deps.len(), 2, "two episodes, not four");
        assert_eq!(deps[0].turns, 1, "one ask in episode one, counted once");
        assert_eq!(deps[0].replies, 1, "one reply, counted once");
    }

    /// C4 — THE HAND-OFF TAX. No public benchmark measures what a provider switch costs; this measures the real
    /// one where it is paid.
    #[test]
    fn the_two_kinds_of_move_are_never_averaged_together() {
        let ev = |sw: bool, ms: i64, tok: i64| format!(
            r#"{{"ev":{{"type":"antiloki","subtype":"handoff","switch":{sw},"ms":{ms},"tokens":{tok}}}}}"#);
        let log = [
            ev(true, 4000, 6000),          // a provider switch: cold start, whole transcript
            ev(true, 6000, 8000),
            ev(false, 900, 1000),          // an in-place restart: same engine, fresh window
            r#"{"ev":{"type":"antiloki","subtype":"user","text":"hi"}}"#.to_string(),
            "not json at all".to_string(),
        ].join("\n");
        let t = super::handoff_tax(log.lines());

        // THE WHOLE POINT OF SPLITTING THEM: a switch costs ~5s and 7k tokens here, an in-place restart under a
        // second and 1k. One mean over both would be a number describing neither, and the chameleon's stay-put
        // rule trades on exactly that difference.
        assert_eq!(t["switch"]["n"], 2);
        assert_eq!(t["switch"]["mean_ms"], 5000);
        assert_eq!(t["switch"]["mean_tokens"], 7000);
        assert_eq!(t["in_place"]["n"], 1);
        assert_eq!(t["in_place"]["mean_ms"], 900);

        // A MEAN OF NOTHING IS NOT ZERO — until a hand-off happens the answer is n=0, and a screen must say
        // "not measured" rather than print a reassuring 0 ms.
        let empty = super::handoff_tax("".lines());
        assert_eq!(empty["switch"]["n"], 0);
        assert_eq!(empty["switch"]["mean_ms"], 0, "the mean is 0 because n is 0 — the caller reads n, not ms");
    }

    use super::*;
    /// F-01 (audit 2026-10-01): the publish nonce is required, per pane, single-use and bounded in time.
    #[test]
    fn the_publish_nonce_is_required_per_pane_single_use_and_expires() {
        let ttl = PUBLISH_NONCE_TTL;
        // none issued for this pane
        assert!(publish_nonce_redeem(9_001, "anything", ttl).is_err());
        let n = publish_nonce_issue(9_002);
        assert_eq!(n.len(), 32);
        // a missing, wrong or another pane's nonce is refused - and a wrong guess does not spend the real one
        assert!(publish_nonce_redeem(9_002, "", ttl).is_err());
        assert!(publish_nonce_redeem(9_002, "0000", ttl).is_err());
        assert!(publish_nonce_redeem(9_003, &n, ttl).is_err());
        assert!(publish_nonce_redeem(9_002, &n, ttl).is_ok(), "the right nonce for the right pane");
        assert!(publish_nonce_redeem(9_002, &n, ttl).is_err(), "one use only");
        // a newer preflight voids the older nonce
        let a = publish_nonce_issue(9_004); let b = publish_nonce_issue(9_004);
        assert_ne!(a, b);
        assert!(publish_nonce_redeem(9_004, &a, ttl).is_err());
        assert!(publish_nonce_redeem(9_004, &b, ttl).is_ok());
        // expiry: a nonce older than the ttl is refused (ttl 0 = every nonce is already old)
        let c = publish_nonce_issue(9_005);
        assert!(publish_nonce_redeem(9_005, &c, std::time::Duration::ZERO).unwrap_err().contains("expired"));
        assert!(publish_nonce_redeem(9_005, &c, ttl).is_err(), "an expired nonce is spent too");
    }

    fn line(seq: u64, t: i64, ev: Value) -> String { json!({ "seq": seq, "t": t, "ev": ev }).to_string() }
    fn user(seq: u64, t: i64, text: &str) -> String { line(seq, t, json!({ "type": "antiloki", "subtype": "user", "text": text })) }
    fn reply(seq: u64, t: i64, text: &str) -> String { line(seq, t, json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] } })) }
    #[test]
    fn an_antiloki_brief_is_a_turn_but_never_the_first_ask() {
        let log = [
            line(1, 100, json!({ "type": "antiloki", "subtype": "started", "engine": "claude-code" })),
            user(2, 110, "[antiloki] A person wants to make a new team, with you as its leader."), reply(3, 120, "which name?"),
            user(4, 125, "[team memory · Calc Crew] This is what the team has done"), reply(5, 126, "ok"),
            user(6, 130, "Calc Crew, fix calc.js"), reply(7, 140, "ready"),
        ].join("\n");
        // a specialist's brief never names an episode either, and a paste tag is stripped from the ask that does
        let log2 = [line(1, 100, json!({ "type": "antiloki", "subtype": "started", "engine": "claude-code" })),
            user(2, 110, "You are Reels Specialist, a specialist. Your purpose: make reels."), reply(3, 120, "ready"),
            user(4, 130, "<pasted_content id=\"cc4c\">\nplan my week"), reply(5, 140, "ok")].join("\n");
        assert_eq!(episodes_of(log2.lines(), "claude-code")[0].first_ask, "plan my week");
        let eps = episodes_of(log.lines(), "claude-code");
        assert_eq!((eps[0].turns, eps[0].first_ask.as_str()), (3, "Calc Crew, fix calc.js"));
    }

    #[test]
    fn episodes_split_at_markers_and_carry_engine_turns_and_the_first_ask() {
        let log = [
            line(1, 100, json!({ "type": "antiloki", "subtype": "started", "engine": "claude-code" })),
            user(2, 110, "what does src/cart.ts do?"), reply(3, 120, "it totals the cart"),
            user(4, 130, "and tests?"), reply(5, 140, "none"),
            line(6, 200, json!({ "type": "antiloki", "subtype": "episode", "reason": "continued on codex", "engine": "codex", "from_engine": "claude-code", "snapshot": "2 turns" })),
            user(7, 210, "go on"), reply(8, 220, "sure"),
        ].join("\n");
        let eps = episodes_of(log.lines(), "codex");
        assert_eq!(eps.len(), 2);
        assert_eq!((eps[0].n, eps[0].engine.as_str(), eps[0].turns, eps[0].first_ask.as_str(), eps[0].last_t, eps[0].reason.as_str()), (1, "claude-code", 2, "what does src/cart.ts do?", 140, "the first"));
        assert_eq!((eps[1].n, eps[1].engine.as_str(), eps[1].turns, eps[1].seq, eps[1].snapshot.as_str()), (2, "codex", 1, 6, "2 turns"));
        // names: the open one from its first ask + reply; a closed one from its marker when it carries them
        assert_eq!((eps[0].title.as_str(), eps[0].subtitle.as_str()), ("what does src/cart.ts do?", "it totals the cart"));
        assert_eq!((eps[1].title.as_str(), eps[1].subtitle.as_str()), ("go on", "sure"));
        let named = [user(1, 1, "a"), reply(2, 2, "b"), line(3, 3, json!({ "type": "antiloki", "subtype": "episode", "reason": "r", "engine": "codex", "from_engine": "codex", "closed_title": "The cart", "closed_subtitle": "What the cart does." }))].join("\n");
        let e = episodes_of(named.lines(), "codex"); assert_eq!((e[0].title.as_str(), e[0].subtitle.as_str(), e[1].title.as_str()), ("The cart", "What the cart does.", ""));
        assert_eq!(title_of("  in one line: what did the QA judge say on the last goal run? and more"), "what did the QA judge say on the last goal run?");
        assert_eq!(title_of("what changed in the cart totals since yesterday and why does the test still fail on CI"), "what changed in the cart totals since yesterday and why do…");
        assert_eq!(subtitle_of("**Run #38** was in box `cockpit-5-0`. It scored 97."), "Run #38 was in box cockpit-5-0.");
        // the snapshot of the first episode: its turns, compacted
        let snap = snapshot_of(log.lines(), 0);
        assert!(snap.starts_with("6 turn(s); the last 6:") && snap.contains("- **you:** what does src/cart.ts do?") && snap.contains("- **antiloki:** it totals the cart"), "{snap}");
        assert_eq!(snapshot_of(log.lines(), 6), "2 turn(s); the last 2:\n- **you:** go on\n- **antiloki:** sure");
        // an empty log: no episodes; a log without markers: one
        assert!(episodes_of("".lines(), "codex").is_empty());
        assert_eq!(episodes_of([user(1, 5, "hi")].join("\n").lines(), "opencode")[0].engine, "opencode");
        // an old log whose `started` had no engine: the marker's from_engine names the first episode
        let old = [line(1, 1, json!({ "type": "antiloki", "subtype": "started" })), user(2, 2, "a"), line(3, 3, json!({ "type": "antiloki", "subtype": "episode", "reason": "continued on opencode", "engine": "opencode", "from_engine": "claude-code" })), user(4, 4, "b")].join("\n");
        assert_eq!(episodes_of(old.lines(), "opencode").iter().map(|e| e.engine.as_str()).collect::<Vec<_>>(), ["claude-code", "opencode"]);
        // an EMPTY episode (a switch right after a switch) is replaced by the next marker, its record carried on
        let twice = [user(1, 1, "a"), reply(2, 2, "b"),
            line(3, 3, json!({ "type": "antiloki", "subtype": "episode", "reason": "continued on opencode", "engine": "opencode", "from_engine": "claude-code", "snapshot": "the record of #1" })),
            line(4, 4, json!({ "type": "antiloki", "subtype": "episode", "reason": "continued on codex", "engine": "codex", "from_engine": "opencode", "snapshot": "the whole handoff" })),
            user(5, 5, "c")].join("\n");
        let e = episodes_of(twice.lines(), "codex");
        assert_eq!(e.iter().map(|x| (x.n, x.engine.as_str(), x.turns, x.snapshot.as_str())).collect::<Vec<_>>(), [(1, "claude-code", 1, ""), (2, "codex", 1, "the record of #1")]);
        // the engine's name lands after the marker and takes over the deterministic one, keyed by n
        let renamed = [twice.clone(), line(6, 6, json!({ "type": "antiloki", "subtype": "episode_named", "n": 1, "title": "The QA judge's verdicts", "subtitle": "Scored per task; failed runs never reach a verdict.", "story": "It began with a question about run 38 and ended on the judge's 97." }))].join("\n");
        let e = episodes_of(renamed.lines(), "codex");
        assert_eq!(e[0].story, "It began with a question about run 38 and ended on the judge's 97."); assert_eq!(e[1].story, "");
        assert_eq!((e[0].title.as_str(), e[0].subtitle.as_str(), e[0].turns, e.len()), ("The QA judge's verdicts", "Scored per task; failed runs never reach a verdict.", 1, 2));
        assert_eq!(parse_episode_name("Sure:\n{\"title\": \"Run 38 and its score.\", \"subtitle\": \"It passed at 97.\"}"), Some(("Run 38 and its score".into(), "It passed at 97".into(), String::new())));
        // the journey (2026-09-23): the story rides beside the name, bounded; it lands on the episode with the name
        let named = parse_episode_name("{\"title\": \"Run 38\", \"subtitle\": \"Passed.\", \"story\": \"The operator asked about run 38. antiloki read the QA log and found 97/100. It ended there.\"}").unwrap();
        assert_eq!(named.2, "The operator asked about run 38. antiloki read the QA log and found 97/100. It ended there");
        assert_eq!(parse_episode_name("{\"subtitle\": \"no title\"}"), None);
        assert_eq!(parse_episode_name("not json"), None);
        // the very first episode, empty, is replaced too
        let fresh = [line(1, 1, json!({ "type": "antiloki", "subtype": "started", "engine": "claude-code" })), line(2, 2, json!({ "type": "antiloki", "subtype": "episode", "reason": "continued on codex", "engine": "codex", "from_engine": "claude-code" })), user(3, 3, "hi")].join("\n");
        let e = episodes_of(fresh.lines(), "codex");
        assert_eq!(e.iter().map(|x| (x.n, x.engine.as_str(), x.turns)).collect::<Vec<_>>(), [(1, "codex", 1)]);
    }
    #[test]
    fn the_boundaries_are_antichats_rules_and_the_preamble_reads_right() {
        let mut ep = Episode { n: 1, seq: 0, t: 0, reason: "".into(), engine: "codex".into(), turns: 3, last_t: 1_000_000, first_ask: "".into(), snapshot: "".into(), title: "".into(), subtitle: "".into(), replies: 0, first_reply: "".into(), story: String::new(), r#box: String::new(), agent: String::new(), forked_from: None };
        assert_eq!(boundary_for(&ep, 1_000_000 + GAP_MS), Some("4h gap".into()));
        assert_eq!(boundary_for(&ep, 1_000_000 + 60_000), None);
        ep.turns = HARD_TURNS; assert_eq!(boundary_for(&ep, 1_000_000 + 1), Some("rotated at 120 turns".into()));
        ep.turns = SOFT_TURNS; assert_eq!(boundary_for(&ep, 1_000_000 + 1), None);
        assert_eq!(boundary_for(&ep, 1_000_000 + PAUSE_MS), Some("rotated at 60 turns, after a 20m pause".into()));
        ep.turns = 0; assert_eq!(boundary_for(&ep, 1_000_000 + GAP_MS * 9), None, "an empty episode never rotates");
        let switch = preamble(&json!({ "reason": "continued on codex", "engine": "codex", "from_engine": "claude-code", "snapshot": "**operator:** hi" }));
        assert!(switch.starts_with("<previous-episode>\nYou are continuing a conversation that ran on claude-code and now continues on codex.") && switch.contains("never change files") && switch.ends_with("</previous-episode>\n\n"));
        // a consolidation on the same engine carries nothing forward — the session went on
        assert_eq!(preamble(&json!({ "reason": "you marked a new topic", "engine": "codex", "from_engine": "codex", "snapshot": "2 turn(s)" })), "");
        // a size boundary restarts the same engine in a fresh window — the conversation rides
        let fresh = preamble(&json!({ "reason": "rotated at 120 turns", "engine": "codex", "from_engine": "codex", "snapshot": "**operator:** hi", "fresh": true }));
        assert!(fresh.contains("on codex in a fresh context window") && fresh.contains("never change files"), "{fresh}");
        assert!(boundary_restarts("rotated at 120 turns") && boundary_restarts("rotated at 60 turns, after a 20m pause") && !boundary_restarts("4h gap") && !boundary_restarts("you marked a new topic"));
        assert_eq!(preamble(&json!({ "reason": "x", "snapshot": "" })), "");
        let a = claude_readonly_args("/w");
        assert_eq!(a, ["--allowedTools", "Read,Grep,Glob,LS,WebFetch,WebSearch,mcp__antiloki", "--disallowedTools", "Edit,Write,MultiEdit,NotebookEdit,Bash", "--add-dir", "/w"]);
        let d = std::env::temp_dir().join(format!("antiloki-chat-harden-{}", std::process::id())); let _ = std::fs::create_dir_all(&d);
        std::fs::write(d.join("opencode.json"), r#"{"model":"x","permission":{"edit":"allow","bash":"ask"}}"#).unwrap();
        harden(&d);
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(d.join("opencode.json")).unwrap()).unwrap();
        assert_eq!((cfg["model"].as_str(), cfg["permission"]["edit"].as_str(), cfg["permission"]["bash"].as_str()), (Some("x"), Some("deny"), Some("deny")));
        let _ = std::fs::remove_dir_all(&d);
    }
    /// operator 2026-09-29: a declined engine hand-off must not keep the OLD ROLE - skills swap in place, only the engine is priced.
    #[test]
    fn a_declined_hand_off_still_changes_the_role_and_keeps_the_engine() {
        use crate::roles::Change;
        let stay = || Change::Stay { why: "staying - Frontender would score 98% against 99%, which this record cannot tell apart".into() };
        match keep_role_on_stay(stay(), "frontender", "Frontender", "backender") {
            Change::Adapter { role, why } => { assert_eq!(role, "frontender"); assert!(why.contains("the engine does not") && why.contains("cannot tell apart"), "{why}"); }
            other => panic!("the role must move: {other:?}"),
        }
        // already that role: nothing to change; and only a Stay is rewritten
        assert!(matches!(keep_role_on_stay(stay(), "backender", "Backender", "backender"), Change::Stay { .. }));
        assert!(matches!(keep_role_on_stay(Change::None, "frontender", "Frontender", "backender"), Change::None));
        assert!(matches!(keep_role_on_stay(stay(), "", "", "backender"), Change::Stay { .. }), "no route role, no change");
    }

    #[test]
    fn a_hand_off_to_a_seat_that_cannot_run_here_becomes_a_stay_that_says_why() {
        use crate::roles::Change;
        let h = || Change::Handoff { role: "backender".into(), engine: "opencode".into(), why: "Backender on opencode — 100% over 3 runs".into() };
        match hold_unreachable_handoff(h(), "Backender", |e| e != "opencode") {
            Change::Stay { why } => { assert!(why.contains("staying")); assert!(why.contains("opencode")); assert!(why.contains("Backender")); }
            other => panic!("expected Stay, got {other:?}"),
        }
        assert!(matches!(hold_unreachable_handoff(h(), "Backender", |_| true), Change::Handoff { .. }), "a reachable hand-off is left alone");
        assert!(matches!(hold_unreachable_handoff(Change::None, "x", |_| false), Change::None), "only a hand-off is held");
    }
    #[test]
    fn a_pane_on_a_switched_off_seat_leaves_it_for_the_heimdall_seat_or_the_first_usable_chat_engine() {
        use crate::cli::Engine::*;
        let off = |e: crate::cli::Engine| e == Opencode;
        let all = |_e: crate::cli::Engine| true;
        assert_eq!(seat_to_leave_for(ClaudeCode, off, all, None), None, "a seat that is on stays");
        assert_eq!(seat_to_leave_for(Opencode, off, all, Some(Codex)), Some(Codex), "the Heimdall seat first");
        assert_eq!(seat_to_leave_for(Opencode, off, all, Some(Gemini)), Some(Gemini), "the Heimdall seat is taken when it can chat — every seat can, since 2026-09-28");
        assert_eq!(seat_to_leave_for(Opencode, off, all, None), Some(Gemini), "no seat chosen: the first in the router's cheapest-first order");
        assert_eq!(seat_to_leave_for(Opencode, off, |e| e == Codex, None), Some(Codex), "…that is actually usable");
        assert_eq!(seat_to_leave_for(Opencode, off, |_| false, None), None, "nothing usable: nowhere to go, the caller says so");
    }
    #[test]
    fn a_pane_whose_seat_is_off_and_has_nowhere_to_go_is_stuck_not_started_anyway() {
        use crate::cli::Engine::*;
        let claude_off = |e: crate::cli::Engine| e == ClaudeCode;
        assert_eq!(leave_decision(Codex, claude_off, |_| true, None), Leave::Stay, "a seat that is on stays");
        assert_eq!(leave_decision(ClaudeCode, claude_off, |e| e == Codex, None), Leave::To(Codex));
        // the Claude-off audit (2026-10-01): with every seat off the pane was STARTED on the switched-off seat after the thread said it could not move - Stuck is what makes the caller refuse
        let all_off = |_e: crate::cli::Engine| true;
        assert_eq!(leave_decision(ClaudeCode, all_off, |_| false, None), Leave::Stuck);
        assert_eq!(leave_decision(Cursor, all_off, |_| true, Some(Codex)), Leave::Stuck, "usable but switched off is still off");
    }

}

#[cfg(test)]
mod scope_fence_tests {
    use super::*;
    fn v(x: &[&str]) -> Vec<String> { x.iter().map(|s| s.to_string()).collect() }

    /// THE WHITELIST IS ENFORCED WHERE THE WORK LANDS (operator 2026-09-30: "the scope is being respected on the agent?").
    #[test]
    fn work_outside_the_scope_is_stopped_before_it_merges_and_named() {
        let scope = v(&["src/ui/**", "docs/README.md"]);
        assert!(scope_block(&scope, &v(&["src/ui/Output.tsx", "src/ui/deep/x.ts", "docs/README.md"])).is_none(), "everything inside: it lands");
        let b = scope_block(&scope, &v(&["src/ui/Output.tsx", "src/server/db.ts", "package.json"])).expect("two files are outside");
        assert_eq!(b["ok"], json!(false)); assert_eq!(b["blocked"], "scope");
        assert_eq!(b["outside"], json!(["src/server/db.ts", "package.json"]));
        let msg = b["error"].as_str().unwrap();
        assert!(msg.contains("2 file(s)") && msg.contains("src/server/db.ts") && msg.contains("nothing was merged") && msg.contains("safe on its branch"), "{msg}");
        // no scope = unrestricted (as it always meant); antiloki's own bookkeeping is never the agent's fault
        assert!(scope_block(&[], &v(&["anything.ts"])).is_none());
        assert!(scope_block(&scope, &v(&[".antiloki/context/INDEX.md", "src/ui/A.tsx"])).is_none());
        // a sibling folder that merely starts with the same letters is NOT inside
        assert!(scope_block(&v(&["src/ui/**"]), &v(&["src/uix/x.ts"])).is_some());
        // "**" is the whole workspace
        assert!(scope_block(&v(&["**"]), &v(&["anything/at/all.ts", "package.json"])).is_none(), "** is the whole workspace: a fork of Main is not fenced out of everything");
    }
    #[test]
    fn a_panes_scope_is_read_from_its_row_whichever_shape_the_row_has() {
        assert_eq!(pane_scope_globs(&json!({ "scope_json": "[\"src/**\",\"docs/**\"]" })), v(&["src/**", "docs/**"]));
        assert_eq!(pane_scope_globs(&json!({ "scope_json": ["a/**"] })), v(&["a/**"]));
        assert!(pane_scope_globs(&json!({ "scope_json": "[]" })).is_empty() && pane_scope_globs(&json!({})).is_empty() && pane_scope_globs(&json!({ "scope_json": "not json" })).is_empty());
        assert_eq!(pane_scope_globs(&json!({ "scope_json": "[\"a/**\",\"  \"]" })), v(&["a/**"]), "blank globs are not globs");
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn two_projects_never_share_a_chat_folder() {
        let a = chat_dir("/home/u/dev/app", 5); let b = chat_dir("/home/u/work/app", 5);
        assert_ne!(a, b, "same folder name, different path");
        assert_eq!(a, chat_dir("/home/u/dev/app/", 5), "a trailing slash is the same project");
        assert_eq!(workspace_key("C:\\Users\\u\\App"), workspace_key("c:/users/u/app"), "windows spelling");
        assert_ne!(chat_dir("/p/a", 1), chat_dir("/p/a", 2));
    }
    #[test]
    fn the_shared_legacy_folder_is_recognised_and_the_scoped_one_is_not() {
        assert!(is_legacy_chat_dir(&chats_dir().join("7").to_string_lossy(), 7));
        assert!(!is_legacy_chat_dir(&chat_dir("/p/a", 7).to_string_lossy(), 7));
        assert!(!is_legacy_chat_dir("", 7));
    }
}
