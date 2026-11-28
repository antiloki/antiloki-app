//! THE PROVIDER OF A PANE, SWITCHED IN THE MIDDLE OF A CONVERSATION (the operator 2026-11-24: "changing a provider closes an episode on the saga and consolidates memory, and the new provider via MCP has to read at least once on
//! start the reconciliation pack"). The button above an agent used to pick a model; it now picks the PROVIDER (the engine of the pane), and the new provider starts on its own default model.
//!
//! What a switch does, in order:
//!  1. brings the workspace's sagas up to date (`work::consolidate`) and CLOSES an episode in the repo's memory (`episodes_db::record`): "switched from A to B", with the conversation so far as its story;
//!  2. writes the RECONCILIATION PACK: what was said and done so far, as the new provider needs it to carry on (the pane's conversation, then the episodes already closed for this agent);
//!  3. changes the pane's engine and forgets the old model (the new provider's own default), then the screen restarts the terminal;
//!  4. when the new terminal's composer is up, types ONE message into it that tells it to call the MCP tool `antiloki_reconcile` first (`after_start`). The tool hands over the pack and marks it READ, so a pane that has
//!     not read it is visible (`status_of`), and the chat says so.
use crate::dispatch::{engine_call, AppState};
use crate::pty::PtyRegistry;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Recon { from: String, pack: String, read: bool, sent: bool, inline: bool, /** the folder it was moved to: the same engine restarted elsewhere, not a switch of provider */ #[serde(default)] moved_to: String }
fn recons() -> &'static Mutex<HashMap<i64, Recon>> { static R: OnceLock<Mutex<HashMap<i64, Recon>>> = OnceLock::new(); R.get_or_init(|| { let mut m: HashMap<i64, Recon> = crate::handover_store::load("reconcile"); for r in m.values_mut() { r.sent = false; } Mutex::new(m) }) }
fn keep(m: &HashMap<i64, Recon>) { crate::handover_store::save("reconcile", m); }

fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }

/// What a provider is called to the person.
pub fn label_of(key: &str) -> String {
    match key { "claude-code" => "Claude Code", "codex" => "Codex", "gemini" => "Antigravity", "cursor" => "Cursor", "grok" => "Grok", "opencode" => "OpenCode", other => other }.to_string()
}

/// What a side of a switch is called: the provider's name, with the local model it runs when it is one (`local:qwen3:4b`). Pure.
pub fn side_label(engine: &str, model: &str) -> String {
    match crate::local_panes::tag_of(model) { Some(tag) => format!("{} on {tag} (local)", label_of(engine)), None => label_of(engine) }
}

/// Why Claude Code cannot be started on this model name, or None when it can: empty (its own default), `local:<tag>`, or a name of Anthropic's. A bare local tag (`gemma4:e4b`) was sent to Anthropic, which refused it only after the switch (#12). Pure.
pub fn claude_model_problem(model: &str) -> Option<String> {
    let m = model.trim().to_lowercase();
    if m.is_empty() || crate::local_panes::tag_of(model).is_some() || m == "default" || m.contains("claude") || ["opus", "sonnet", "haiku", "fable"].iter().any(|f| m.starts_with(f)) { return None; }
    Some(format!("`{}` is not a Claude model. For a model on this machine, write `local:{}`", model.trim(), model.trim()))
}

/// A text cut at `n` characters on a character boundary, with an ellipsis when it was longer, on one line. Pure.
pub fn clip(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= n { one } else { format!("{}…", one.chars().take(n).collect::<String>()) }
}

/// A "person" turn that antiloki or its provider menu typed, not the person: an earlier takeover message, or a setting command (`/model x`, `/effort x`). Pure.
pub fn typed_by_antiloki(turn: &Value) -> bool {
    if turn["role"] != "user" { return false; }
    let t = crate::chat_pane::strip_paste_tag(turn["text"].as_str().unwrap_or(""));
    t.starts_with("[antiloki] You are taking over") || t.starts_with("[antiloki] A person wants to make a new team") || ["/model", "/effort"].iter().any(|c| t.strip_prefix(c).map_or(false, |rest| rest.is_empty() || rest.starts_with(char::is_whitespace)))
}

/// THE RECONCILIATION PACK: the conversation so far (the last turns, oldest first, each cut short; the tools an answer used by name) and the episodes already closed for this agent, written for the provider that takes over. Pure - tested.
pub fn reconcile_pack(from: &str, to: &str, turns: &[Value], episodes: &[Value]) -> String { reconcile_pack_named(&label_of(from), &label_of(to), turns, episodes) }
/// The same pack with the two sides named by what the person sees (a local model is `Claude Code on qwen3:4b (local)`, so a switch to one does not read "Claude Code takes over from Claude Code"). Pure.
pub fn reconcile_pack_named(from_l: &str, to_l: &str, turns: &[Value], episodes: &[Value]) -> String {
    let (from_l, to_l) = (from_l.to_string(), to_l.to_string());
    let mut out = format!("# Reconciliation pack: {to_l} takes over from {from_l}\n\n{from_l} was working with the person in this same workspace and conversation, and the person switched provider in the middle of it. Nothing is lost: this is what was said and done so far, oldest first. Carry on from the last message. Do not start over, and do not ask the person to repeat anything that is written here.\n");
    // WHAT ANTILOKI TYPED ITSELF IS NOT THE CONVERSATION (#23): an earlier takeover message carries a whole earlier pack, so each switch nested the last one inside the next
    let said: Vec<&Value> = turns.iter().filter(|t| !typed_by_antiloki(t)).collect();
    let shown: Vec<&Value> = said[said.len().saturating_sub(14)..].to_vec();
    if shown.is_empty() { out.push_str(&format!("\n## The conversation so far\n(No turns could be read from {from_l}: use the episodes below and the saga, `antiloki_sagas`.)\n")); }
    else {
        out.push_str(&format!("\n## The conversation so far (the last {} of {} turns)\n", shown.len(), said.len()));
        for t in &shown {
            let who = if t["role"] == "user" { "Person".to_string() } else { from_l.clone() };
            let tools: Vec<String> = t["tools"].as_array().into_iter().flatten().filter_map(|x| x["name"].as_str().map(String::from)).collect();
            let used = if tools.is_empty() { String::new() } else { format!(" (tools: {})", tools.join(", ")) };
            out.push_str(&format!("- **{who}**{used}: {}\n", clip(t["text"].as_str().unwrap_or(""), 700)));
        }
        if shown.last().map_or(false, |t| t["role"] == "user") { out.push_str("\nThe last message is the person's and has no answer yet: answer it first.\n"); }
    }
    if !episodes.is_empty() {
        out.push_str("\n## Episodes already closed for this agent (newest first)\n");
        let mut lines: Vec<(String, usize)> = vec![];   // the same switch over and over is said once, with how often (#28)
        for e in episodes { let line = format!("{}{}", clip(e["title"].as_str().unwrap_or(""), 120), e["snapshot"].as_str().filter(|s| !s.is_empty()).map(|s| format!(": {}", clip(s, 240))).unwrap_or_default()); match lines.iter_mut().find(|(l, _)| *l == line) { Some(x) => x.1 += 1, None => lines.push((line, 1)) } }
        for (line, n) in lines.iter().take(6) { out.push_str(&format!("- {line}{}\n", if *n > 1 { format!(" (×{n})") } else { String::new() })); }
    }
    out.push_str("\nThe whole saga of this project is `antiloki_sagas`. Say in one short line that you have reconciled, then carry on.\n");
    out
}

/// The one message typed into the new terminal: read the pack through the MCP tool first. Pure - tested.
pub fn takeover_message(from_label: &str) -> String {
    format!("[antiloki] You are taking over this conversation from {from_label} because the person switched provider. Before anything else, call the antiloki MCP tool `antiloki_reconcile` once and read what it returns; it holds what was said and done so far. Then reply with one short line saying you have reconciled and what you will do next.")
}

/// THE MESSAGE THAT STARTS THE NEW PROVIDER: the call to `antiloki_reconcile`; for a LOCAL MODEL the pack itself rides in it too (cut at 6000 characters), because a small model did not call the tool (2026-11-24, gemma4:e4b and lfm3:1.7b both answered with a greeting). Pure - tested.
pub fn takeover_text(from_label: &str, pack: &str, inline: bool) -> String {
    if !inline { return takeover_message(from_label); }
    let cut: String = pack.chars().take(6000).collect();
    format!("{}\n\nThe reconciliation pack, so you do not need the tool:\n\n{cut}", takeover_message(from_label))
}

/// `Some("pending")` until the new provider has called `antiloki_reconcile`, then `Some("read")`; `None` for a pane nobody switched.
pub fn status_of(pane_id: i64) -> Option<&'static str> { recons().lock().ok()?.get(&pane_id).map(|r| if r.read { "read" } else { "pending" }) }

/// The pack for the MCP tool, marking it read. `None` when nobody switched this pane.
pub fn read_pack(pane_id: i64) -> Option<String> {
    let mut m = recons().lock().ok()?; let r = m.get_mut(&pane_id)?; r.read = true; let pack = r.pack.clone(); keep(&m); Some(pack)
}

/// `POST /api/v1/cockpit/pane/provider {pane_id, engine}`: switch the pane's provider (see the module). The answer says `restart: true`: the screen starts the terminal again, which starts the new provider.
pub async fn provider_post(state: Arc<AppState>, goals: Arc<crate::goals::Goals>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = b["pane_id"].as_i64().unwrap_or(0); let to = b["engine"].as_str().unwrap_or("").trim().to_string();
    // a LOCAL MODEL is Claude Code with its brain on the model server: the same provider, a different model, and the same handover (the episode and the pack) as any other switch
    let model = b["model"].as_str().unwrap_or("").trim().to_string();
    let Some(engine) = crate::cli::engine_of(&json!(to)) else { return reply(json!({ "ok": false, "error": format!("`{to}` is not a provider antiloki can run") })) };
    if engine == crate::cli::Engine::ClaudeCode { if let Some(why) = claude_model_problem(&model) { return reply(json!({ "ok": false, "error": why })); } }
    if engine.bin_present().is_none() { return reply(json!({ "ok": false, "error": format!("{} is not installed on this machine", label_of(&to)) })); }
    // a local model runs through Claude Code with the model server as its brain: it needs no sign-in to Claude, so a signed-out Claude does not block it
    let local = crate::local_panes::tag_of(&model).is_some();
    if !local && crate::cli::engine_seat(engine) == Some(false) { return reply(json!({ "ok": false, "error": format!("{} is installed but not signed in: sign in to it once in a terminal, then switch", label_of(&to)) })); }
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let Some(pane) = engine_call(&state, "system.cockpit.pane.get", json!({ "id": id }), ctx.clone()).await.ok().map(|(v, _)| v).filter(|v| !v.is_null()) else { return reply(json!({ "ok": false, "error": "no such pane" })) };
    let from = pane["pane_kind"].as_str().unwrap_or("").to_string();
    if crate::cli::engine_of(&json!(from)).is_none() { return reply(json!({ "ok": false, "error": "this pane is not an agent" })); }
    if from == to && model.is_empty() { return reply(json!({ "ok": true, "changed": false, "engine": to })); }

    // 1+2: what was said, and the episodes already closed for this agent
    let (root, _, _) = crate::cowork::pane_facts(&state, id).await;
    let turns: Vec<Value> = crate::cowork::transcript_value(&state, id, 60).await["turns"].as_array().cloned().unwrap_or_default();
    let slug = pane["subagent_name"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| format!("pane-{id}"));
    let (r1, s1) = (root.clone(), slug.clone());
    let earlier = tokio::task::spawn_blocking(move || crate::episodes_db::list(&r1, Some(&s1), 6)).await.unwrap_or_default();
    let from_model = crate::pane_model::model_of(pane["settings_json"].as_str().unwrap_or(""));
    let (from_label, to_label) = (side_label(&from, &from_model), side_label(&to, &model));
    let settings_text = pane["settings_json"].as_str().unwrap_or("").to_string();
    let pack = crate::team_onboarding::handover(&settings_text, &reconcile_pack_named(&from_label, &to_label, &turns, &earlier));
    if crate::team_onboarding::is_onboarding(&settings_text) { crate::team_onboarding::drop_waiting(id); }

    // 1: the sagas up to date, and an episode closed in the repo's memory with the pack as its story
    let (st2, g2, r2) = (state.clone(), goals.clone(), root.clone());
    let _ = tokio::task::spawn_blocking(move || crate::work::consolidate(&st2, &g2, &r2, id)).await;
    tokio::spawn(crate::specialists::distill_pane(state.clone(), id));
    let (r3, s3, from3, fl3, tl3, pack3, n_turns) = (root.clone(), slug.clone(), from.clone(), from_label.clone(), to_label.clone(), pack.clone(), turns.len());
    let closed = tokio::task::spawn_blocking(move || {
        let n = crate::episodes_db::list(&r3, Some(&s3), 10_000).iter().filter_map(|e| e["n"].as_i64()).max().unwrap_or(0) as usize + 1;
        let now = crate::cli::now_ms();
        crate::episodes_db::record(&r3, &crate::episodes_db::Closed { agent_slug: s3.clone(), pane_id: id, n, engine: from3.clone(), title: format!("Switched provider: {fl3} to {tl3}"), subtitle: "the conversation went on with another provider".into(),
            snapshot: format!("{fl3} handed this conversation to {tl3}"), story: pack3, first_t: now, last_t: now, turns: n_turns, ..Default::default() })
    }).await.unwrap_or_else(|e| Err(e.to_string()));

    // the pack on disk beside the pane's other files, for anyone who wants to read it
    let _ = std::fs::create_dir_all(std::path::Path::new(&root).join(".antiloki")); let _ = std::fs::write(std::path::Path::new(&root).join(".antiloki").join("reconcile.md"), &pack);

    // 3: the new engine, on its own default model
    let mut settings: Value = serde_json::from_str(pane["settings_json"].as_str().unwrap_or("{}")).unwrap_or(json!({})); if !settings.is_object() { settings = json!({}); }
    settings.as_object_mut().unwrap().remove("model");
    if !model.is_empty() { settings["model"] = json!(model); }
    if let Err(e) = engine_call(&state, "system.cockpit.pane.update", json!({ "pane_id": id, "pane_kind": to, "settings": settings }), ctx).await { return reply(json!({ "ok": false, "error": e })); }
    // the pane's own context is baked again for its NEW engine: a worktree shared by panes keeps one opencode.json, and it must name THIS pane's id (found 2026-11-26: after a switch to OpenCode the MCP server still named an earlier pane, so the takeover could not reach its own pane)
    let _ = engine_call(&state, "system.cockpit.pane.context.bake", json!({ "pane_id": id }), json!({ "actor": "web-fe", "source": "web-fe" })).await;
    if let Ok(mut m) = recons().lock() { m.insert(id, Recon { from: from_label.clone(), pack: pack.clone(), read: false, sent: false, inline: local, moved_to: String::new() }); keep(&m); }
    reply(json!({ "ok": true, "changed": true, "from": from, "engine": to, "label": label_of(&to), "restart": true, "episode_closed": closed.is_ok(), "pack_chars": pack.chars().count() }))
}

/// An agent that is about to be restarted in ANOTHER FOLDER, on the same engine: Codex, Cursor, Antigravity and OpenCode keep conversations by folder, so what was said goes with it as a handover, read by the new terminal like after a switch.
pub async fn carry_conversation(state: &Arc<AppState>, pane_id: i64, pane: &Value, to_folder: &str) {
    let turns: Vec<Value> = crate::cowork::transcript_value(state, pane_id, 60).await["turns"].as_array().cloned().unwrap_or_default();
    if turns.is_empty() { return; }
    let engine = pane["pane_kind"].as_str().unwrap_or("").to_string(); let model = crate::pane_model::model_of(pane["settings_json"].as_str().unwrap_or(""));
    let label = side_label(&engine, &model);
    let pack = reconcile_pack_named(&label, &label, &turns, &[]);
    if let Ok(mut m) = recons().lock() { m.insert(pane_id, Recon { from: label, pack, read: false, sent: false, inline: crate::local_panes::tag_of(&model).is_some(), moved_to: to_folder.to_string() }); keep(&m); }
}

/// What an agent restarted in another folder is told first. Pure - tested.
pub fn moved_text(folder: &str, pack: &str, inline: bool) -> String {
    let head = format!("[antiloki] You were restarted in another folder, {folder}, because the person moved your thread's work there. Paths from earlier in the conversation are in the old folder: work in the new one. Before anything else, call the antiloki MCP tool `antiloki_reconcile` once and read what it returns; it holds what was said and done so far. Then reply with one short line.");
    if !inline { return head; }
    format!("{head}\n\nThe reconciliation pack, so you do not need the tool:\n\n{}", pack.chars().take(6000).collect::<String>())
}

/// Called when a pane's terminal has just been started: if the pane was switched and the pack is unread, type the takeover message once the composer is up.
pub fn after_start(reg: Arc<PtyRegistry>, pane_id: i64) {
    let (from, pack, inline, moved_to) = match recons().lock() { Ok(mut m) => match m.get_mut(&pane_id) { Some(r) if !r.sent && !r.read => { r.sent = true; (r.from.clone(), r.pack.clone(), r.inline, r.moved_to.clone()) } _ => return }, Err(_) => return };
    tokio::spawn(async move {
        let text = if moved_to.is_empty() { takeover_text(&from, &pack, inline) } else { moved_text(&moved_to, &pack, inline) };
        let r = crate::pty::pty_send(reg, Bytes::from(json!({ "pane_id": pane_id, "text": text, "wait_s": 90 }).to_string())).await;
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap_or_default(); let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if v["ok"] != json!(true) { if let Ok(mut m) = recons().lock() { if let Some(x) = m.get_mut(&pane_id) { x.sent = false; } } }   // not typed: the next start tries again
        else if inline { if let Ok(mut m) = recons().lock() { if let Some(x) = m.get_mut(&pane_id) { x.read = true; } keep(&m); } }   // the pack went in with the message: it is read
    });
}

#[cfg(test)]
mod tests {
    #[test] fn a_moved_agent_is_told_the_folder_and_never_that_the_provider_changed() {
        let t = super::moved_text("/trees/pricing", "PACK", false); assert!(t.contains("/trees/pricing") && t.contains("antiloki_reconcile") && !t.contains("switched provider") && !t.contains("PACK"));
        assert!(super::moved_text("/p", "PACK", true).ends_with("PACK"));
    }
    use super::*;
    fn turn(role: &str, text: &str, tools: &[&str]) -> Value { json!({ "role": role, "text": text, "tools": tools.iter().map(|t| json!({ "name": t })).collect::<Vec<_>>() }) }
    #[test]
    fn the_pack_tells_the_new_provider_what_was_said_who_said_it_and_what_is_still_unanswered() {
        let turns = vec![turn("user", "fix the login bug", &[]), turn("assistant", "Looking at it.", &["Read", "Bash"]), turn("user", "and add a test", &[])];
        let p = reconcile_pack("claude-code", "codex", &turns, &[json!({ "title": "Earlier ask", "snapshot": "done" })]);
        assert!(p.contains("Codex takes over from Claude Code"), "{p}");
        assert!(p.contains("**Person**: fix the login bug") && p.contains("**Claude Code** (tools: Read, Bash): Looking at it."), "{p}");
        assert!(p.contains("answer it first"), "the last message is the person's: {p}");
        assert!(p.contains("Earlier ask: done") && p.contains("antiloki_sagas"));
    }
    #[test]
    fn a_local_model_gets_the_pack_in_the_message_and_a_cloud_provider_only_the_pointer_to_the_tool() {
        let big = "x".repeat(9000);
        let cloud = takeover_text("Claude Code", "THE PACK", false); assert!(cloud.contains("antiloki_reconcile") && !cloud.contains("THE PACK"));
        let local = takeover_text("Claude Code", "THE PACK", true); assert!(local.contains("antiloki_reconcile") && local.contains("THE PACK"));
        assert!(takeover_text("Claude Code", &big, true).chars().count() < 6000 + takeover_message("Claude Code").chars().count() + 200, "bounded");
    }
    #[test]
    fn a_switch_to_a_local_model_names_it_instead_of_claude_taking_over_from_claude() {
        assert_eq!(side_label("claude-code", "local:qwen3:4b"), "Claude Code on qwen3:4b (local)");
        assert_eq!(side_label("claude-code", "haiku"), "Claude Code"); assert_eq!(side_label("codex", ""), "Codex");
        let p = reconcile_pack_named(&side_label("claude-code", "haiku"), &side_label("claude-code", "local:qwen3:4b"), &[turn("user", "hi", &[])], &[]);
        assert!(p.contains("Claude Code on qwen3:4b (local) takes over from Claude Code"), "{p}");
    }
    #[test]
    fn only_a_claude_name_or_a_local_one_starts_claude_code() {
        for ok in ["", "haiku", "sonnet[1m]", "Opus", "claude-sonnet-5-5", "us.anthropic.claude-opus-5-5", "fable", "default", "local:gemma4:e4b"] { assert_eq!(claude_model_problem(ok), None, "{ok}"); }
        let why = claude_model_problem("gemma4:e4b").unwrap(); assert!(why.contains("local:gemma4:e4b"), "{why}");
    }
    #[test]
    fn a_pack_with_no_readable_turns_says_so_and_points_at_the_saga() {
        let p = reconcile_pack("codex", "claude-code", &[], &[]);
        assert!(p.contains("No turns could be read from Codex") && p.contains("antiloki_sagas"), "{p}");
    }
    #[test]
    fn what_antiloki_typed_itself_never_enters_the_next_pack() {
        let first = reconcile_pack("claude-code", "codex", &[turn("user", "the codename is KESTREL-9", &[]), turn("assistant", "noted", &[])], &[]);
        let after = [turn("user", "the codename is KESTREL-9", &[]), turn("assistant", "noted", &[]), turn("user", &takeover_text("Claude Code", &first, true), &[]),
            turn("assistant", "reconciled", &[]), turn("user", "<pasted_content id=\"1\">[antiloki] You are taking over this conversation from Codex", &[]), turn("user", "/model haiku", &[]), turn("user", "/models are listed where?", &[])];
        let p = reconcile_pack("codex", "claude-code", &after, &[]);
        assert!(p.contains("KESTREL-9") && p.contains("reconciled") && p.contains("/models are listed where?"), "{p}");
        assert!(!p.contains("You are taking over") && !p.contains("/model haiku"), "{p}");
        assert!(p.contains("the last 4 of 4 turns"), "{p}");
        assert!(!typed_by_antiloki(&turn("assistant", "[antiloki] You are taking over", &[])));
    }
    #[test]
    fn a_long_turn_is_cut_and_only_the_last_turns_are_shown() {
        let turns: Vec<Value> = (0..30).map(|i| turn("user", &format!("message {i} {}", "x".repeat(2000)), &[])).collect();
        let p = reconcile_pack("claude-code", "codex", &turns, &[]);
        assert!(p.contains("the last 14 of 30 turns") && p.contains("message 29") && !p.contains("message 15 ") && p.contains('…'));
        assert!(p.chars().count() < 14 * 800 + 1500, "bounded: {}", p.chars().count());
    }
    #[test]
    fn the_takeover_message_names_the_tool_and_the_pack_is_read_once_then_marked() {
        assert!(takeover_message("Claude Code").contains("antiloki_reconcile") && takeover_message("Claude Code").contains("Claude Code"));
        let id = 987_650;
        assert_eq!(status_of(id), None, "nobody switched this pane"); assert_eq!(read_pack(id), None);
        recons().lock().unwrap().insert(id, Recon { from: "claude-code".into(), pack: "THE PACK".into(), read: false, sent: false, inline: false, moved_to: String::new() });
        assert_eq!(status_of(id), Some("pending")); assert_eq!(read_pack(id).as_deref(), Some("THE PACK")); assert_eq!(status_of(id), Some("read"));
    }
}
