//! THE CHAT SKIN'S DATA (the operator 2026-11-21: "two themes for the agent panes: the terminal and a chat style; the agents are still terminals, it is just UI"): the pane's own Claude Code transcript as plain turns.
//! The terminal keeps running underneath; this only READS what the engine already wrote (`~/.claude/projects/<slug>/<session>.jsonl`), so the chat view shows the same text, tools and tokens as the terminal. Pure over the lines.
use serde_json::{json, Value};

fn brief_of(name: &str, input: &Value) -> String {
    let pick = |k: &str| input[k].as_str().map(String::from);
    let s = match name { "Bash" => pick("command"), "Read" | "Edit" | "Write" | "NotebookEdit" | "MultiEdit" => pick("file_path"), "Grep" | "Glob" => pick("pattern"), "WebFetch" | "WebSearch" => pick("url").or_else(|| pick("query")), "Task" => pick("description"), _ => input.as_object().and_then(|o| o.values().find_map(|v| v.as_str().map(String::from))) };
    s.unwrap_or_default().replace('\n', " ").chars().take(120).collect()
}
/// The CLI wraps a pasted message in `<pasted_content …>` tags: the person never typed them, so the chat does not show them. Pure.
fn strip_paste_tags(t: &str) -> String { let mut out = String::new(); let mut rest = t; while let Some(i) = rest.find("<pasted_content").or_else(|| rest.find("</pasted_content")) { out.push_str(&rest[..i]); match rest[i..].find('>') { Some(j) => rest = &rest[i + j + 1..], None => { rest = ""; } } } out.push_str(rest); out.trim().to_string() }
fn user_text(v: &Value) -> String {
    let c = &v["message"]["content"]; if let Some(s) = c.as_str() { return strip_paste_tags(s); }
    strip_paste_tags(&c.as_array().map(|a| a.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default())
}
fn noise(t: &str) -> bool { let t = t.trim(); t.is_empty() || t.starts_with("<system-reminder>") || t.starts_with("<command-") || t.starts_with("<local-command") || t.starts_with("Caveat:") || t.contains("[Request interrupted by user") }
/// Did the PERSON write this `user` line? Claude Code writes its own under the same name: the summary it makes when a conversation is shortened (operator 2026-11-28: it showed as a long message of theirs) and a background task's notice. Pure.
fn persons(v: &Value) -> bool { v["isCompactSummary"] != json!(true) && v["isVisibleInTranscriptOnly"] != json!(true) && !v["origin"]["kind"].as_str().is_some_and(|k| k != "human") }
/// The conversation as turns, oldest first: `{role: "user" | "assistant", t, text, tools: [{name, brief, ok}], model, tokens_in, tokens_out}`. Sidechains and meta lines are left out; consecutive assistant records of one answer are one turn; a tool's result marks its call as failed or not. At most `cap` turns (the latest). Pure.
pub fn turns_from_claude<'a>(lines: impl Iterator<Item = &'a str>, cap: usize) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["isSidechain"] == json!(true) || v["isMeta"] == json!(true) { continue; }
        let ts = v["timestamp"].as_str().unwrap_or("").to_string();
        match v["type"].as_str() {
            Some("user") => {
                // a tool result closes its call
                if let Some(a) = v["message"]["content"].as_array() { for b in a.iter().filter(|b| b["type"] == "tool_result") { let id = b["tool_use_id"].as_str().unwrap_or(""); let bad = b["is_error"] == json!(true); for t in out.iter_mut().rev() { if let Some(ts) = t["tools"].as_array_mut() { if let Some(c) = ts.iter_mut().find(|c| c["id"] == json!(id)) { c["ok"] = json!(!bad); c["done"] = json!(true); break; } } } } }
                if !persons(&v) { continue; }
                let text = user_text(&v); if noise(&text) { continue; }
                out.push(json!({ "role": "user", "t": ts, "text": text.trim(), "tools": [] }));
            }
            Some("assistant") => {
                let m = &v["message"]; let mut text = String::new(); let mut tools = vec![];
                for b in m["content"].as_array().into_iter().flatten() { match b["type"].as_str() { Some("text") => { if let Some(x) = b["text"].as_str() { text.push_str(x); } } Some("tool_use") => tools.push(json!({ "id": b["id"], "name": b["name"], "brief": brief_of(b["name"].as_str().unwrap_or(""), &b["input"]), "done": false, "ok": true })), _ => {} } }
                if text.trim().is_empty() && tools.is_empty() { continue; }
                let (tin, tout) = (m["usage"]["input_tokens"].as_u64().unwrap_or(0) + m["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0) + m["usage"]["cache_creation_input_tokens"].as_u64().unwrap_or(0), m["usage"]["output_tokens"].as_u64().unwrap_or(0));
                if let Some(last) = out.last_mut().filter(|l| l["role"] == "assistant" && l["msg"] == m["id"] && !m["id"].is_null()) { let prev = last["text"].as_str().unwrap_or("").to_string(); last["text"] = json!(format!("{prev}{text}")); if let Some(a) = last["tools"].as_array_mut() { a.extend(tools); } last["tokens_out"] = json!(tout.max(last["tokens_out"].as_u64().unwrap_or(0))); continue; }
                out.push(json!({ "role": "assistant", "t": ts, "msg": m["id"], "text": text.trim(), "tools": tools, "model": m["model"], "tokens_in": tin, "tokens_out": tout }));
            }
            // SENT WHILE IT WORKED (operator 2026-11-28: the chat showed the message under later ones and "Reasoning" never stopped): Claude Code writes no `user` line for it, only this, where the agent took it in
            Some("attachment") => { if let Some(text) = queued_prompt(&v["attachment"]) { out.push(json!({ "role": "user", "t": ts, "text": text, "tools": [] })); } }
            _ => {}
        }
    }
    let from = out.len().saturating_sub(cap); out.split_off(from)
}

/// What the PERSON typed while Claude was working, from its `queued_command` attachment. A task's notice or another agent's message arrives the same way and is not theirs. Pure.
fn queued_prompt(a: &Value) -> Option<String> {
    if a["type"] != "queued_command" || a["commandMode"] != "prompt" || a["origin"]["kind"].as_str().is_some_and(|k| k != "human") { return None; }
    let text = user_text(&json!({ "message": { "content": a["prompt"] } }));
    (!noise(&text)).then_some(text)
}

/// A CURSOR TRANSCRIPT AS TURNS (issue 8): `~/.cursor/projects/<slug>/agent-transcripts/<id>/<id>.jsonl`, one `{role, message:{content:[{type:text|tool_use,…}]}}` per line. The person's text sits inside `<user_query>`;
/// consecutive assistant lines are one answer. Pure - tested.
pub fn turns_from_cursor<'a>(lines: impl Iterator<Item = &'a str>, cap: usize) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let blocks = v["message"]["content"].as_array().cloned().unwrap_or_default();
        let text: String = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n");
        match v["role"].as_str() {
            Some("user") => {
                let t = match (text.find("<user_query>"), text.find("</user_query>")) { (Some(a), Some(b)) if b > a => text[a + 12..b].to_string(), _ => text };
                if noise(&t) { continue; }
                out.push(json!({ "role": "user", "t": "", "text": t.trim(), "tools": [] }));
            }
            Some("assistant") => {
                let tools: Vec<Value> = blocks.iter().filter(|b| b["type"] == "tool_use").map(|b| { let n = b["name"].as_str().unwrap_or("tool"); json!({ "name": n, "brief": brief_of(n, &json!({ "file_path": b["input"]["path"], "command": b["input"]["command"], "pattern": b["input"]["pattern"] })), "done": true, "ok": true }) }).collect();
                if text.trim().is_empty() && tools.is_empty() { continue; }
                if let Some(last) = out.last_mut().filter(|l| l["role"] == "assistant") {
                    let prev = last["text"].as_str().unwrap_or("").to_string(); last["text"] = json!(format!("{prev}\n{}", text.trim()).trim().to_string());
                    if let Some(a) = last["tools"].as_array_mut() { a.extend(tools); }
                } else { out.push(json!({ "role": "assistant", "t": "", "text": text.trim(), "tools": tools })); }
            }
            _ => {}
        }
    }
    let from = out.len().saturating_sub(cap); out.split_off(from)
}
/// An OPENCODE SESSION AS TURNS (issue 8): its database holds `message` rows (JSON with `role`) and `part` rows (`text`, `tool`) per message. `msgs` is `(message json, [part json])`, oldest first. Pure - tested.
/// OPENCODE 2's STORE (`session_message` rows of type user / assistant; an assistant's pieces in `session_part`, or inline in `content`) in the shape `turns_from_opencode` reads. `parts` are the message's
/// `session_part` rows in order; empty when it has none. Pure.
pub fn opencode_v2_message(kind: &str, data: &Value, parts: Vec<Value>) -> (Value, Vec<Value>) {
    let mut parts = if parts.is_empty() { data["content"].as_array().cloned().unwrap_or_default() } else { parts };
    if kind == "user" { parts = vec![json!({ "type": "text", "text": data["text"].as_str().unwrap_or("") })]; }
    for p in parts.iter_mut() { if p["type"] == json!("tool") && p["tool"].is_null() { p["tool"] = p["name"].clone(); } }
    let mut m = json!({ "role": kind, "time": data["time"].clone(), "modelID": data["model"]["id"].clone() });
    if let Some(e) = data["error"]["message"].as_str() { m["error"] = json!({ "name": data["error"]["type"].as_str().unwrap_or("error"), "data": { "message": e } }); }
    (m, parts)
}
pub fn turns_from_opencode(msgs: &[(Value, Vec<Value>)], cap: usize) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for (m, parts) in msgs {
        let text: String = parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n");
        let t = m["time"]["created"].as_i64().map(|ms| ms.to_string()).unwrap_or_default();
        match m["role"].as_str() {
            Some("user") => { if noise(&text) { continue; } out.push(json!({ "role": "user", "t": t, "text": text.trim(), "tools": [] })); }
            Some("assistant") => {
                let tools: Vec<Value> = parts.iter().filter(|p| p["type"] == "tool").map(|p| { let n = p["tool"].as_str().unwrap_or("tool"); let ok = p["state"]["status"] != json!("error"); json!({ "name": n, "brief": brief_of(n, &json!({ "file_path": p["state"]["input"]["filePath"], "command": p["state"]["input"]["command"], "pattern": p["state"]["input"]["pattern"] })), "done": p["state"]["status"] != json!("running"), "ok": ok }) }).collect();
                // a turn the provider refused (expired key, no quota…) has no text, only `error.data.message`: show it, or the chat sits on "Reasoning" forever (seen live 2026-11-25). A person's own abort is not news.
                let err = (m["error"]["name"] != json!("MessageAbortedError")).then(|| m["error"]["data"]["message"].as_str().or_else(|| m["error"]["name"].as_str())).flatten().map(|e| format!("⚠ {}", e.trim()));
                let text = if text.trim().is_empty() { err.unwrap_or_default() } else { text };
                if text.trim().is_empty() && tools.is_empty() { continue; }
                out.push(json!({ "role": "assistant", "t": t, "text": text.trim(), "tools": tools, "model": m["modelID"] }));
            }
            _ => {}
        }
    }
    let from = out.len().saturating_sub(cap); out.split_off(from)
}

/// A CODEX ROLLOUT AS TURNS (2026-11-24: the chat was Claude-only; Codex writes `~/.codex/sessions/**/rollout-*.jsonl`, one JSON event per line). The person's messages and the assistant's answers are kept; the
/// injected instructions (AGENTS.md, environment, developer notes) and the reasoning are left out; each tool call is joined to its output by `call_id`. Pure - tested.
/// ANTIGRAVITY'S OWN TRANSCRIPT (`~/.gemini/antigravity-cli/brain/<conversation>/.system_generated/logs/transcript.jsonl`, seen on agy 1.5.18): one step per line. The person's words come wrapped in
/// `<USER_REQUEST>`; a model step has `content` and / or `tool_calls`; the step that follows a call is its result. Steps that follow each other from the model are one turn. Pure.
pub fn turns_from_agy<'a>(lines: impl Iterator<Item = &'a str>, cap: usize) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let t = v["created_at"].as_str().unwrap_or("").to_string(); let content = v["content"].as_str().unwrap_or("").trim();
        match (v["source"].as_str().unwrap_or(""), v["type"].as_str().unwrap_or("")) {
            (_, "USER_INPUT") => {
                let text = content.split("<USER_REQUEST>").nth(1).map(|r| r.split("</USER_REQUEST>").next().unwrap_or(r)).unwrap_or(content).trim();
                if !text.is_empty() { out.push(json!({ "role": "user", "t": t, "text": text, "tools": [] })); }
            }
            ("MODEL", "PLANNER_RESPONSE") => {
                let calls: Vec<Value> = v["tool_calls"].as_array().into_iter().flatten().map(|c| {
                    let brief: String = c["args"].as_object().and_then(|a| a.values().find_map(|x| x.as_str())).unwrap_or("").trim_matches('"').chars().take(120).collect();
                    json!({ "name": c["name"].as_str().unwrap_or("tool"), "brief": brief, "done": false, "ok": true })
                }).collect();
                if content.is_empty() && calls.is_empty() { continue; }
                // an answer that follows tool calls is shown after them, not above
                let after_tools = !content.is_empty() && calls.is_empty() && out.last().is_some_and(|l| l["role"] == "assistant" && l["tools"].as_array().is_some_and(|a| !a.is_empty()));
                if after_tools || !out.last().is_some_and(|l| l["role"] == "assistant") { out.push(json!({ "role": "assistant", "t": t, "text": "", "tools": [], "model": v["model"].as_str().unwrap_or("") })); }
                let last = out.last_mut().unwrap();
                if !content.is_empty() { let prev = last["text"].as_str().unwrap_or(""); last["text"] = json!(format!("{prev}\n{content}").trim()); }
                if let Some(tools) = last["tools"].as_array_mut() { tools.extend(calls); }
            }
            // the result of the oldest call still open
            ("MODEL", "GENERIC") => { if let Some(tool) = out.last_mut().and_then(|l| l["tools"].as_array_mut()).and_then(|a| a.iter_mut().find(|x| x["done"] == json!(false))) { tool["done"] = json!(true); } }
            _ => {}
        }
    }
    let n = out.len(); if n > cap { out.drain(0..n - cap); }
    out
}
pub fn turns_from_codex<'a>(lines: impl Iterator<Item = &'a str>, cap: usize) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    let mut calls: std::collections::HashMap<String, (usize, usize)> = std::collections::HashMap::new(); // call_id -> (turn, tool index)
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != json!("response_item") { continue; }
        let p = &v["payload"]; let ts = v["timestamp"].as_str().unwrap_or("").to_string();
        let text_of = |p: &Value| p["content"].as_array().map(|a| a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
        match p["type"].as_str() {
            Some("message") => {
                let text = text_of(p);
                match p["role"].as_str() {
                    Some("user") => {
                        if noise(&text) || text.starts_with("# AGENTS.md") || text.starts_with("<environment") || text.starts_with("<INSTRUCTIONS") { continue; }
                        out.push(json!({ "role": "user", "t": ts, "text": text.trim(), "tools": [] }));
                    }
                    Some("assistant") => {
                        if text.trim().is_empty() { continue; }
                        // an answer that follows tool calls of the same run joins that turn, as the Claude reader joins one message's parts
                        if let Some(last) = out.last_mut().filter(|l| l["role"] == "assistant" && l["tools"].as_array().map_or(false, |t| !t.is_empty())) {
                            let prev = last["text"].as_str().unwrap_or("").to_string(); last["text"] = json!(format!("{prev}\n{}", text.trim()).trim().to_string());
                        } else { out.push(json!({ "role": "assistant", "t": ts, "text": text.trim(), "tools": [] })); }
                    }
                    _ => {}
                }
            }
            Some("custom_tool_call") | Some("function_call") => {
                let name = p["name"].as_str().unwrap_or("tool").to_string();
                let raw = p["input"].as_str().map(String::from).unwrap_or_else(|| p["arguments"].as_str().map(String::from).unwrap_or_default());
                let brief: String = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(120).collect();
                if out.last().map_or(true, |l| l["role"] != "assistant") { out.push(json!({ "role": "assistant", "t": ts, "text": "", "tools": [] })); }
                let turn = out.len() - 1;
                let tools = out[turn]["tools"].as_array_mut().expect("tools");
                tools.push(json!({ "name": name, "brief": brief, "done": false, "ok": Value::Null }));
                if let Some(id) = p["call_id"].as_str() { calls.insert(id.to_string(), (turn, tools.len() - 1)); }
            }
            Some("custom_tool_call_output") | Some("function_call_output") => {
                let Some((turn, idx)) = p["call_id"].as_str().and_then(|id| calls.get(id).copied()) else { continue };
                let output = p["output"].as_str().map(String::from).unwrap_or_else(|| p["output"].to_string());
                // the exec tool reports "Process exited with code N": anything but 0 is a failure
                let failed = output.find("Process exited with code ").map_or(false, |at| !output[at + 25..].starts_with('0'));
                if let Some(t) = out[turn]["tools"].as_array_mut().and_then(|t| t.get_mut(idx)) { t["done"] = json!(true); t["ok"] = json!(!failed); }
            }
            _ => {}
        }
    }
    let n = out.len(); if n > cap { out.drain(0..n - cap); }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn what_claude_code_writes_by_itself_is_not_shown_as_the_persons_message() {
        let lines = [
            r#"{"type":"user","timestamp":"2026-11-28T10:00:00Z","origin":{"kind":"human"},"message":{"content":"mine"}}"#,
            r#"{"type":"user","timestamp":"2026-11-28T10:00:01Z","isCompactSummary":true,"isVisibleInTranscriptOnly":true,"message":{"content":"This session is being continued from a previous conversation"}}"#,
            r#"{"type":"user","timestamp":"2026-11-28T10:00:02Z","origin":{"kind":"task-notification"},"message":{"content":"<task-notification>done</task-notification>"}}"#,
            r#"{"type":"user","timestamp":"2026-11-28T10:00:03Z","message":{"content":"an older line with no origin"}}"#,
        ];
        let said: Vec<String> = turns_from_claude(lines.into_iter(), 20).iter().map(|t| t["text"].as_str().unwrap_or("").to_string()).collect();
        assert_eq!(said, vec!["mine".to_string(), "an older line with no origin".to_string()]);
    }
    #[test]
    fn opencode_two_messages_become_the_same_turns_with_parts_tools_and_a_providers_error() {
        use super::{opencode_v2_message as m, turns_from_opencode};
        let msgs = vec![
            m("user", &json!({ "text": "hi", "time": { "created": 1 } }), vec![]),
            m("assistant", &json!({ "content": [], "model": { "id": "free" }, "time": { "created": 2 } }), vec![json!({ "type": "reasoning", "text": "hidden" }), json!({ "type": "tool", "name": "read", "state": { "status": "completed", "input": { "filePath": "/a" } } }), json!({ "type": "text", "text": "Hi!" })]),
            m("assistant", &json!({ "content": [], "error": { "type": "provider.quota", "message": "Insufficient balance" }, "time": { "created": 3 } }), vec![]),
        ];
        let t = turns_from_opencode(&msgs, 10);
        assert_eq!((t[0]["text"].as_str(), t[1]["text"].as_str(), t[1]["tools"][0]["name"].as_str()), (Some("hi"), Some("Hi!"), Some("read")));
        assert!(t[2]["text"].as_str().unwrap().contains("Insufficient balance"), "{t:?}");
    }
    #[test]
    fn antigravitys_steps_become_turns_with_the_request_unwrapped_and_its_calls_closed() {
        let l = [
            r#"{"source":"USER_EXPLICIT","type":"USER_INPUT","created_at":"t1","content":"<USER_REQUEST>\nsay hi\n</USER_REQUEST>\n<ADDITIONAL_METADATA>x</ADDITIONAL_METADATA>"}"#,
            r#"{"source":"MODEL","type":"PLANNER_RESPONSE","created_at":"t2","model":"g","tool_calls":[{"name":"read_file","args":{"Path":"\"/a\""}}]}"#,
            r#"{"source":"MODEL","type":"GENERIC","content":"result"}"#,
            r#"{"source":"SYSTEM","type":"SYSTEM_MESSAGE","content":"hidden"}"#,
            r#"{"source":"MODEL","type":"PLANNER_RESPONSE","created_at":"t3","content":"hi"}"#,
        ];
        let t = super::turns_from_agy(l.iter().copied(), 10);
        assert_eq!(t.len(), 3, "the calls, then the answer below them: {t:?}");
        assert_eq!((t[0]["text"].as_str(), t[1]["text"].as_str(), t[2]["text"].as_str()), (Some("say hi"), Some(""), Some("hi")));
        assert_eq!((t[1]["tools"][0]["name"].as_str(), t[1]["tools"][0]["brief"].as_str(), t[1]["tools"][0]["done"].as_bool()), (Some("read_file"), Some("/a"), Some(true)));
    }

    use super::*;
    const T: &str = r#"{"type":"user","timestamp":"2026-11-21T10:00:00Z","message":{"content":"fix the login bug"}}
{"type":"user","isMeta":true,"timestamp":"2026-11-21T10:00:01Z","message":{"content":"<command-name>/clear</command-name>"}}
{"type":"assistant","timestamp":"2026-11-21T10:00:02Z","message":{"id":"m1","model":"claude-sonnet-5-5","content":[{"type":"text","text":"Looking at it."}],"usage":{"input_tokens":10,"cache_read_input_tokens":90,"output_tokens":5}}}
{"type":"assistant","timestamp":"2026-11-21T10:00:03Z","message":{"id":"m1","model":"claude-sonnet-5-5","content":[{"type":"tool_use","id":"tu1","name":"Read","input":{"file_path":"src/login.ts"}}],"usage":{"input_tokens":10,"output_tokens":20}}}
{"type":"user","timestamp":"2026-11-21T10:00:04Z","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"..."}]}}
{"type":"assistant","isSidechain":true,"timestamp":"2026-11-21T10:00:05Z","message":{"id":"s1","content":[{"type":"text","text":"a sub-agent talking"}]}}
{"type":"assistant","timestamp":"2026-11-21T10:00:06Z","message":{"id":"m2","content":[{"type":"tool_use","id":"tu2","name":"Bash","input":{"command":"npm test\n-- --run"}}],"usage":{"output_tokens":7}}}
{"type":"user","timestamp":"2026-11-21T10:00:07Z","message":{"content":[{"type":"tool_result","tool_use_id":"tu2","is_error":true,"content":"fail"}]}}
{"type":"assistant","timestamp":"2026-11-21T10:00:08Z","message":{"id":"m3","content":[{"type":"text","text":"The test fails."}],"usage":{"output_tokens":4}}}
{"type":"user","timestamp":"2026-11-21T10:00:09Z","message":{"content":"[Request interrupted by user]"}}
garbage {"#;
    #[test] fn the_transcript_becomes_turns_with_tools_that_know_if_they_failed() {
        let t = turns_from_claude(T.lines(), 100); let roles: Vec<&str> = t.iter().map(|x| x["role"].as_str().unwrap()).collect(); assert_eq!(roles, vec!["user", "assistant", "assistant", "assistant"], "meta, sidechain, interruption note and garbage are left out: {t:?}");
        assert_eq!(t[0]["text"], json!("fix the login bug")); assert_eq!(t[1]["text"], json!("Looking at it.")); assert_eq!(t[1]["tools"][0]["name"], json!("Read")); assert_eq!(t[1]["tools"][0]["brief"], json!("src/login.ts")); assert_eq!(t[1]["tools"][0]["ok"], json!(true)); assert_eq!(t[1]["tools"][0]["done"], json!(true), "one answer's records are one turn");
        assert_eq!(t[2]["tools"][0]["brief"], json!("npm test -- --run")); assert_eq!(t[2]["tools"][0]["ok"], json!(false)); assert_eq!(t[1]["tokens_in"], json!(100)); assert_eq!(t[3]["text"], json!("The test fails."));
    }
    #[test] fn a_message_sent_while_claude_worked_is_a_turn_where_it_was_taken_in() {
        let l = [r#"{"type":"user","message":{"content":"fix it"}}"#, r#"{"type":"assistant","message":{"id":"a","content":[{"type":"text","text":"On it."}]}}"#,
            r#"{"type":"queue-operation","operation":"enqueue","content":"and push"}"#, r#"{"type":"attachment","timestamp":"t2","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"and push"}}"#,
            r#"{"type":"attachment","attachment":{"type":"queued_command","commandMode":"task-notification","prompt":"a task finished"}}"#, r#"{"type":"attachment","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"peer"},"prompt":"from another agent"}}"#,
            r#"{"type":"attachment","attachment":{"type":"queued_command","commandMode":"prompt","prompt":[{"type":"text","text":"with a picture"}]}}"#, r#"{"type":"assistant","message":{"id":"b","content":[{"type":"text","text":"Pushed."}]}}"#];
        let t = turns_from_claude(l.into_iter(), 20); let said: Vec<(&str, &str)> = t.iter().map(|x| (x["role"].as_str().unwrap(), x["text"].as_str().unwrap())).collect();
        assert_eq!(said, vec![("user", "fix it"), ("assistant", "On it."), ("user", "and push"), ("user", "with a picture"), ("assistant", "Pushed.")]);
    }
    #[test] fn only_the_latest_turns_are_kept() { let t = turns_from_claude(T.lines(), 2); assert_eq!(t.len(), 2); assert_eq!(t[1]["text"], json!("The test fails.")); }
    #[test] fn the_pasted_content_tags_of_the_cli_are_not_shown() {
        assert_eq!(strip_paste_tags("hello <pasted_content id=\"b1\">the brief</pasted_content id=\"b1\"> bye"), "hello the brief bye"); assert_eq!(strip_paste_tags("plain"), "plain");
        let t = turns_from_claude(r#"{"type":"user","timestamp":"t","message":{"content":"<pasted_content id=\"x\">Say ready.</pasted_content id=\"x\">"}}"#.lines(), 5); assert_eq!(t[0]["text"], json!("Say ready."));
    }
    #[test]
    fn a_codex_rollout_is_the_person_and_the_answers_with_each_tool_joined_to_its_output() {
        let rollout = [
            r##"{"timestamp":"t0","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<skills_instructions>x"}]}}"##,
            r##"{"timestamp":"t1","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /w"}]}}"##,
            r##"{"timestamp":"t2","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"This project is codenamed OTTER-7."}]}}"##,
            r##"{"timestamp":"t3","type":"response_item","payload":{"type":"reasoning","summary":[]}}"##,
            r##"{"timestamp":"t4","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c1","input":"const x = 1;\nconsole.log(x)"}}"##,
            r##"{"timestamp":"t5","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c1","output":"Script completed\nOutput:\n1\nProcess exited with code 0"}}"##,
            r##"{"timestamp":"t6","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c2","input":"boom()"}}"##,
            r##"{"timestamp":"t7","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"c2","output":"Process exited with code 1"}}"##,
            r##"{"timestamp":"t8","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Noted: OTTER-7."}]}}"##,
        ];
        let t = turns_from_codex(rollout.iter().copied(), 10);
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!(t[0]["role"], json!("user")); assert_eq!(t[0]["text"], json!("This project is codenamed OTTER-7."), "the injected instructions are left out");
        assert_eq!(t[1]["role"], json!("assistant")); assert_eq!(t[1]["text"], json!("Noted: OTTER-7."));
        assert_eq!(t[1]["tools"][0]["name"], json!("exec")); assert_eq!(t[1]["tools"][0]["brief"], json!("const x = 1;"));
        assert_eq!((t[1]["tools"][0]["done"].clone(), t[1]["tools"][0]["ok"].clone()), (json!(true), json!(true)));
        assert_eq!(t[1]["tools"][1]["ok"], json!(false), "a non-zero exit is a failed tool");
        assert_eq!(turns_from_codex(rollout.iter().copied(), 1).len(), 1, "the cap keeps the newest");
    }
    #[test]
    fn a_cursor_transcript_gives_the_query_and_one_joined_answer_with_its_tools() {
        let l = [r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>x</timestamp>\n<user_query>\nfix it\n</user_query>"}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Looking."},{"type":"tool_use","name":"Read","input":{"path":"/a/b.ts"}}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#, r#"{"type":"turn_ended","status":"success"}"#];
        let t = turns_from_cursor(l.iter().copied(), 50);
        assert_eq!(t.len(), 2); assert_eq!(t[0]["text"], json!("fix it")); assert_eq!(t[1]["text"], json!("Looking.\nDone.")); assert_eq!(t[1]["tools"][0]["name"], json!("Read"));
        assert_eq!(turns_from_cursor(l.iter().copied(), 1).len(), 1);
    }
    #[test]
    fn an_opencode_session_gives_its_user_and_assistant_messages_with_tool_parts() {
        let m = vec![(json!({"role":"user","time":{"created":1}}), vec![json!({"type":"text","text":"hello"})]),
            (json!({"role":"assistant","modelID":"qwen"}), vec![json!({"type":"text","text":"hi"}), json!({"type":"tool","tool":"bash","state":{"status":"error","input":{"command":"ls"}}})])];
        let t = turns_from_opencode(&m, 50);
        assert_eq!(t.len(), 2); assert_eq!(t[0]["text"], json!("hello")); assert_eq!(t[1]["tools"][0]["ok"], json!(false)); assert_eq!(t[1]["tools"][0]["brief"], json!("ls")); assert_eq!(t[1]["model"], json!("qwen"));
    }
    #[test]
    fn an_opencode_turn_the_provider_refused_shows_its_error_and_a_person_s_abort_shows_nothing() {
        let m = vec![(json!({"role":"user","time":{"created":1}}), vec![json!({"type":"text","text":"hi"})]),
            (json!({"role":"assistant","error":{"name":"APIError","data":{"message":"This API key has expired."}}}), vec![]),
            (json!({"role":"user","time":{"created":2}}), vec![json!({"type":"text","text":"again"})]),
            (json!({"role":"assistant","error":{"name":"MessageAbortedError","data":{"message":"Aborted"}}}), vec![])];
        let t = turns_from_opencode(&m, 50);
        assert_eq!(t.len(), 3); assert_eq!(t[1]["text"], json!("⚠ This API key has expired.")); assert_eq!(t[2]["role"], json!("user"));
    }
}
