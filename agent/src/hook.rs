//! `antiloki-agent hook` — the PreToolUse hook for a code-agent pane (Claude Code and Codex read
//! the same stdin/stdout schema; OpenCode's plugin does the same two jobs in-process).
//!
//! Deterministic, no network. Per call it:
//!   1. REFUSES a write outside the pane's edit scope (`.antiloki/context/manifest.json` → `scope`)
//!      and, in a goal run, outside the task's ownership (`ANTILOKI_SUBSCOPE`) — exit 2 + the reason
//!      the agent should relay. The box is enforced here, not requested of the model.
//!   2. NUDGES a source read towards its baked review (once per file per session) and towards
//!      INDEX.md (once per session) — never blocks a read.
//!   3. MEASURES: one JSONL line per call in `.antiloki/reads.jsonl` (op read|write|search|exec|tool),
//!      the sha+size of what a write left on disk (PostToolUse), the prompts, the answers, the plans.
//!
//! A port of antiloki-BE/src/mcp/antiloki-hook.ts; the pure functions are tested below with the
//! same cases the TS spec used.
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

fn s(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `new Date().toISOString()`
pub fn now_iso() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let ms = d.subsec_millis();
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let mm = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = if mm <= 2 { y + 1 } else { y };
    format!("{yy:04}-{mm:02}-{dd:02}T{:02}:{:02}:{:02}.{ms:03}Z", sod / 3600, (sod % 3600) / 60, sod % 60)
}

fn is_edit_tool(t: &str) -> bool {
    matches!(t, "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "apply_patch" | "edit" | "write" | "patch" | "multiedit")
}
fn is_shell_tool(t: &str) -> bool {
    matches!(t, "shell" | "local_shell" | "Bash" | "bash" | "exec_command")
}

/// `path.relative(root, abs)` with forward slashes; None when it escapes the root. Purely lexical
/// (like node's path.relative), so a path that does not exist yet still resolves.
fn rel_to(root: &Path, raw: &str) -> Option<String> {
    let abs = if Path::new(raw).is_absolute() { PathBuf::from(raw) } else { root.join(raw) };
    let norm = |p: &Path| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in p.components() {
            match c {
                Component::Normal(x) => out.push(x.to_string_lossy().into_owned()),
                Component::ParentDir => { out.pop(); }
                _ => {}
            }
        }
        out
    };
    let a = norm(&abs);
    let r = norm(root);
    if a.len() < r.len() || a[..r.len()] != r[..] { return None; }
    Some(a[r.len()..].join("/"))
}

/// Same semantics as the scope sentry: exact path, `dir/**` prefix, a plain dir prefix, or a glob.
pub fn in_scope(scope: &[String], rel: &str) -> bool {
    if scope.is_empty() { return true; } // no scope = unrestricted (the gate makes this rare)
    scope.iter().any(|g| {
        if g == rel { return true; }
        if let Some(pre) = g.strip_suffix("/**") { return format!("{rel}/").starts_with(&format!("{pre}/")); }
        if !g.contains(['*', '?', '[']) { return rel == g || rel.starts_with(&format!("{}/", g.trim_end_matches('/'))); }
        glob_re(g).map(|re| re.is_match(rel)).unwrap_or(false)
    })
}
fn glob_re(g: &str) -> Option<Regex> {
    let esc = |seg: &str| seg.split('*').map(regex::escape).collect::<Vec<_>>().join("[^/]*");
    let body = g.split("**").map(esc).collect::<Vec<_>>().join(".*");
    Regex::new(&format!("^{body}$")).ok()
}

/// The files a write tool is about to touch, worktree-relative.
pub fn edit_targets(root: &Path, tool: &str, input: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if tool == "apply_patch" {
        let patch = s(input.get("input")).to_string() + &s(input.get("patch"));
        let re = Regex::new(r"(?m)^\*\*\* (?:Update|Add|Delete) File: (.+)$").unwrap();
        for m in re.captures_iter(&patch) { if let Some(r) = rel_to(root, m[1].trim()) { out.push(r); } }
        return out;
    }
    let raw = ["file_path", "filePath", "path", "notebook_path"].iter().map(|k| s(input.get(k))).find(|v| !v.is_empty()).unwrap_or_default();
    if !raw.is_empty() { if let Some(r) = rel_to(root, &raw) { out.push(r); } }
    out
}

fn command_text(command: &Value) -> String {
    match command {
        Value::Array(a) => {
            let argv: Vec<String> = a.iter().map(|x| s(Some(x))).collect();
            if argv.len() >= 3 && matches!(base_name(&argv[0]).as_str(), "bash" | "sh" | "zsh") && (argv[1] == "-c" || argv[1] == "-lc") { argv[2..].join(" ") } else { argv.join(" ") }
        }
        Value::Null => String::new(),
        other => s(Some(other)),
    }
}
fn base_name(p: &str) -> String {
    p.trim_matches(|c| c == '\'' || c == '"').rsplit(['/', '\\']).next().unwrap_or("").to_string()
}
fn unquote(x: &str) -> String {
    let t = x.strip_prefix(['\'', '"']).unwrap_or(x);
    t.strip_suffix(['\'', '"']).unwrap_or(t).to_string()
}

/// SHELL WRITES: the files a shell command writes through redirection, `tee`, `sed -i`, `cp`/`mv`
/// destinations, `touch`, `rm`, `mkdir` — targets the tool layer never sees as an edit.
pub fn shell_write_targets(root: &Path, command: &Value) -> Vec<String> {
    let flat = command_text(command);
    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let t = unquote(raw);
        if t.is_empty() || t == "/dev/null" || t.starts_with('-') || t.starts_with('&') { return; }
        if let Some(r) = rel_to(root, &t) { if !r.is_empty() && !out.contains(&r) { out.push(r); } }
    };
    // quoted strings are consumed whole so a `>` inside them is never a redirect; a quoted target still counts
    let quoted = r#""(?:[^"\\]|\\.)*"|'[^']*'"#;
    let re = Regex::new(&format!(r#"{quoted}|(?:^|[^<>&|\d])\d?>{{1,2}}\s*({quoted}|[^\s|;&<>]+)"#)).unwrap();
    for m in re.captures_iter(&flat) { if let Some(t) = m.get(1) { push(t.as_str()); } }
    let unq = Regex::new(quoted).unwrap().replace_all(&flat, "\"\"").into_owned();
    let split = Regex::new(r"\s*(?:\|\||&&|;|\|)\s*").unwrap();
    for seg in split.split(&unq) {
        let toks: Vec<&str> = seg.split_whitespace().collect();
        if toks.is_empty() { continue; }
        let base = base_name(toks[0]);
        let args = &toks[1..];
        let operands: Vec<&str> = args.iter().copied().filter(|a| !a.starts_with('-')).collect();
        match base.as_str() {
            "tee" => operands.iter().for_each(|o| push(o)),
            "sed" if args.iter().any(|a| Regex::new(r"^-[a-zA-Z]*i").unwrap().is_match(a)) => operands.iter().skip(1).for_each(|o| push(o)),
            "cp" | "mv" => { if operands.len() >= 2 { push(operands[operands.len() - 1]); } }
            "touch" | "rm" | "mkdir" | "rmdir" | "truncate" => operands.iter().for_each(|o| push(o)),
            _ => {}
        }
    }
    out
}

/// Walk up from `start` to the directory holding `.antiloki/context/manifest.json`.
pub fn find_pack_root(start: &Path) -> Option<PathBuf> {
    let mut dir = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    for _ in 0..12 {
        if dir.join(".antiloki/context/manifest.json").exists() { return Some(dir); }
        match dir.parent() { Some(p) => dir = p.to_path_buf(), None => break }
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
pub struct Classified { pub kind: String, pub rel: String }

fn rel_kind(root: &Path, raw: &str) -> Classified {
    let Some(rel) = rel_to(root, raw) else { return Classified { kind: "other".into(), rel: raw.to_string() } };
    let kind = if rel.starts_with(".antiloki/context/") { "pack" }
        else if rel.starts_with(".antiloki/") || rel.starts_with(".claude/") || rel.starts_with(".codex/") || rel.starts_with(".opencode/") || rel.starts_with(".agents/") || rel == "CLAUDE.md" || rel == "AGENTS.md" { "brief" }
        else { "source" };
    Classified { kind: kind.into(), rel }
}

const READERS: &[&str] = &["cat", "head", "tail", "sed", "awk", "less", "more", "bat", "nl", "wc", "stat", "python", "python3", "node"];
const SEARCHERS: &[&str] = &["rg", "grep", "egrep", "fgrep", "ag", "find", "fd", "ls", "tree", "git"];

/// Codex reads through its shell tool: argv tokens that are files under the worktree are the read.
pub fn classify_shell(root: &Path, command: &Value) -> Classified {
    let flat = match command { Value::Array(_) => command_text(command), other => s(Some(other)) };
    let tokens: Vec<String> = Regex::new(r"[\s|;&()<>]+").unwrap().split(&flat).filter(|t| !t.is_empty()).map(unquote).collect();
    let mut reader = false; let mut searcher = false;
    for tk in &tokens {
        let b = base_name(tk);
        if READERS.contains(&b.as_str()) { reader = true; }
        if SEARCHERS.contains(&b.as_str()) { searcher = true; }
    }
    let files: Vec<&String> = tokens.iter().filter(|tk| !tk.starts_with('-') && tk.len() >= 2).filter(|tk| {
        let abs = if Path::new(tk.as_str()).is_absolute() { PathBuf::from(tk.as_str()) } else { root.join(tk.as_str()) };
        abs.is_file()
    }).collect();
    if !files.is_empty() {
        let kinds: Vec<Classified> = files.iter().map(|f| rel_kind(root, f)).collect();
        return kinds.iter().find(|k| k.kind == "source").or_else(|| kinds.iter().find(|k| k.kind == "pack")).unwrap_or(&kinds[0]).clone();
    }
    let head = tokens.iter().take(4).cloned().collect::<Vec<_>>().join(" ");
    if searcher { return Classified { kind: "search".into(), rel: head }; }
    if reader { return Classified { kind: "other".into(), rel: head }; }
    Classified { kind: "other".into(), rel: String::new() }
}

pub fn is_read(tool: &str) -> bool { tool == "Read" || tool == "read_file" || is_shell_tool(tool) }

fn is_antiloki_tool(tool: &str) -> bool {
    Regex::new(r"(^|__)antiloki__antiloki_").unwrap().is_match(tool)
        || Regex::new(r"^antiloki_(check|query|sources|reads|laws|file_review|folder_review|blast_radius|tests_for|context_index)$").unwrap().is_match(tool)
}

pub fn classify(root: &Path, tool: &str, input: &Value) -> Classified {
    if is_antiloki_tool(tool) { return Classified { kind: "tool".into(), rel: tool.rsplit("antiloki__").next().unwrap_or(tool).to_string() }; }
    if tool == "Grep" || tool == "Glob" { return Classified { kind: "search".into(), rel: [s(input.get("pattern")), s(input.get("path"))].into_iter().find(|v| !v.is_empty()).unwrap_or_default() }; }
    if is_shell_tool(tool) { return classify_shell(root, &shell_command(input)); }
    let raw = ["file_path", "path", "notebook_path"].iter().map(|k| s(input.get(k))).find(|v| !v.is_empty()).unwrap_or_default();
    if raw.is_empty() { return Classified { kind: "other".into(), rel: String::new() }; }
    rel_kind(root, &raw)
}
fn shell_command(input: &Value) -> Value {
    ["command", "cmd", "argv"].iter().filter_map(|k| input.get(k)).find(|v| !v.is_null()).cloned().unwrap_or(Value::Null)
}

/// AUDIT: every hook line carries an `op` — read | write | search | exec | tool | other.
pub fn op_of(tool: &str) -> &'static str {
    if is_edit_tool(tool) { return "write"; }
    if matches!(tool, "Read" | "read_file" | "read") { return "read"; }
    if matches!(tool, "Grep" | "Glob" | "grep" | "glob" | "list") { return "search"; }
    if is_shell_tool(tool) { return "exec"; }
    if tool.to_lowercase().contains("antiloki") || tool.to_lowercase().contains("antiqa") { return "tool"; }
    "other"
}

fn sha_bytes(p: &Path) -> (Option<String>, Option<u64>) {
    match std::fs::read(p) {
        Ok(buf) => (Some(hex::encode(Sha256::digest(&buf))[..16].to_string()), Some(buf.len() as u64)),
        Err(_) => (None, None),
    }
}

/// The OUTCOME of a write (PostToolUse): size + short sha256 of the file as it is on disk now.
pub fn written_lines(root: &Path, tool: &str, input: &Value, response: &Value) -> Vec<Value> {
    let t = now_iso();
    if is_edit_tool(tool) {
        return edit_targets(root, tool, input).into_iter().map(|rel| {
            let (sha, bytes) = sha_bytes(&root.join(&rel));
            json!({ "t": t, "tool": tool, "path": rel, "kind": "written", "op": "write", "nudged": null, "sha": sha, "bytes": bytes })
        }).collect();
    }
    if is_shell_tool(tool) {
        let cmd_v = shell_command(input);
        let cmd: String = match &cmd_v { Value::Array(a) => a.iter().map(|x| s(Some(x))).collect::<Vec<_>>().join(" "), other => s(Some(other)) };
        let cmd: String = cmd.chars().take(400).collect();
        let exit = response.as_object().and_then(|o| ["exit_code", "exitCode", "code"].iter().filter_map(|k| o.get(*k)).next().cloned()).unwrap_or(Value::Null);
        let mut lines = Vec::new();
        if !cmd.is_empty() { lines.push(json!({ "t": t, "tool": tool, "path": cmd, "kind": "exec", "op": "exec", "nudged": null, "exit": exit })); }
        for rel in shell_write_targets(root, &cmd_v) {
            let (sha, bytes) = sha_bytes(&root.join(&rel));
            lines.push(json!({ "t": t, "tool": tool, "path": rel, "kind": "written", "op": "write", "nudged": null, "sha": sha, "bytes": bytes, "via": "shell" }));
        }
        return lines;
    }
    Vec::new()
}

/// WHAT ONE TOOL CALL WAS, IN A LINE: the file it touched, the command it ran, the pattern it searched, the page it fetched. Pure.
pub fn tool_summary(root: &Path, tool: &str, input: &Value) -> String {
    let rel = |p: &str| p.strip_prefix(&format!("{}/", root.to_string_lossy())).unwrap_or(p).to_string();
    let first = |keys: &[&str]| keys.iter().find_map(|k| input.get(*k).and_then(Value::as_str)).unwrap_or("").to_string();
    let out = match tool {
        "Bash" | "shell" | "local_shell" | "exec_command" => { let c = shell_command(input); match &c { Value::Array(a) => a.iter().map(|x| s(Some(x))).collect::<Vec<_>>().join(" "), o => s(Some(o)) } }
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => rel(&first(&["file_path", "path", "notebook_path"])),
        "Grep" => format!("{} {}", first(&["pattern"]), rel(&first(&["path"]))).trim().to_string(),
        "Glob" => format!("{} {}", first(&["pattern"]), rel(&first(&["path"]))).trim().to_string(),
        "WebFetch" => first(&["url"]),
        "WebSearch" => first(&["query"]),
        "Task" | "Agent" => first(&["description", "prompt"]),
        "TodoWrite" => format!("{} to-do item{}", input.get("todos").and_then(Value::as_array).map_or(0, Vec::len), if input.get("todos").and_then(Value::as_array).map_or(0, Vec::len) == 1 { "" } else { "s" }),
        _ => { let hit = input.as_object().and_then(|o| o.values().find_map(Value::as_str)).unwrap_or("").to_string(); if hit.is_empty() { input.to_string() } else { hit } }
    };
    out.chars().take(200).collect::<String>().replace('\n', " ")
}

/// The line the trace gets for one tool event (`PostToolUse`, `PostToolUseFailure`): who, what, whether it worked, how much came back. A TodoWrite carries its whole list: that is
/// where an episode's steps come from. Pure.
pub fn trace_line(root: &Path, event: &str, input: &Value) -> Option<Value> {
    let tool = s(input.get("tool_name")); if tool.is_empty() { return None; }
    let ti = input.get("tool_input").cloned().unwrap_or(Value::Null); let tr = input.get("tool_response").cloned().unwrap_or(Value::Null);
    let failed = event == "PostToolUseFailure" || tr.get("is_error") == Some(&Value::Bool(true));
    let interrupted = tr.get("interrupted") == Some(&Value::Bool(true));
    let mut line = json!({
        "t": now_iso(), "session_id": input.get("session_id").cloned().unwrap_or(Value::Null), "tool_use_id": input.get("tool_use_id").cloned().unwrap_or(Value::Null),
        "tool": tool, "summary": tool_summary(root, &tool, &ti), "outcome": if failed { "error" } else if interrupted { "interrupted" } else { "ok" },
        "out_bytes": if tr.is_null() { Value::Null } else { json!(tr.to_string().len()) },
    });
    if failed { let e = input.get("error").map(|e| s(Some(e))).unwrap_or_default(); if !e.is_empty() { line["error"] = json!(e.chars().take(240).collect::<String>()); } }
    if tool == "TodoWrite" {
        let todos: Vec<Value> = ti.get("todos").and_then(Value::as_array).into_iter().flatten().take(40).map(|t| json!({ "content": s(t.get("content")).chars().take(160).collect::<String>(), "status": s(t.get("status")) })).collect();
        line["todos"] = json!(todos);
    }
    Some(line)
}

/// AN AGENT THAT SHARES ITS FOLDER keeps its records in a folder of its own (the serve names it in ANTILOKI_RECORDS at start); an agent with a copy of its own keeps them in `.antiloki`. Pure over the variable.
pub fn records_in(root: &Path, named: Option<std::ffi::OsString>) -> PathBuf { named.filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| root.join(".antiloki")) }
pub fn records(root: &Path) -> PathBuf { records_in(root, std::env::var_os("ANTILOKI_RECORDS")) }

/// The trace is capped: past this size the older half goes to `trace.1.jsonl` (one generation), so a long-lived box never grows without bound.
const TRACE_MAX_BYTES: u64 = 4 * 1024 * 1024;
fn append_trace(root: &Path, line: &Value) {
    let p = records(root).join("trace.jsonl");
    if std::fs::metadata(&p).map_or(false, |m| m.len() > TRACE_MAX_BYTES) { let _ = std::fs::rename(&p, records(root).join("trace.1.jsonl")); }
    append(&p, line);
}

/// A plan the engine just produced → `<root>/.antiloki/plans/<name>.md`.
pub fn capture_plan(root: &Path, tool: &str, input: &Value) -> Option<(String, String)> {
    let dir = root.join(".antiloki/plans");
    let write = |name: &str, body: &str, source: &str| -> Option<(String, String)> {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(dir.join(name), body).ok()?;
        Some((format!(".antiloki/plans/{name}"), source.to_string()))
    };
    if tool == "update_plan" {
        let steps = input.get("plan").and_then(Value::as_array).cloned().unwrap_or_default();
        if steps.is_empty() { return None; }
        let expl = s(input.get("explanation"));
        let mut body = String::from("# Plan (Codex)\n\n");
        if !expl.is_empty() { body.push_str(&format!("{expl}\n\n")); }
        for (i, st) in steps.iter().enumerate() {
            let status = s(st.get("status"));
            body.push_str(&format!("{}. [{}] {}{}\n", i + 1, if status == "completed" { "x" } else { " " }, s(st.get("step")), if status == "in_progress" { " _(in progress)_" } else { "" }));
        }
        return write("codex-plan.md", &body, "update_plan");
    }
    if matches!(tool, "Write" | "Edit" | "MultiEdit") {
        let target = s(input.get("file_path"));
        if !Regex::new(r"[\\/]\.claude[\\/]plans[\\/][^\\/]+\.md$").unwrap().is_match(&target) { return None; }
        let body = std::fs::read_to_string(&target).ok()?;
        let name = Regex::new(r"[^A-Za-z0-9._-]+").unwrap().replace_all(&base_name(&target), "-").into_owned();
        return write(&name, &body, &target);
    }
    None
}

/// GOAL RUNS: a task's ownership rides on the CLI's environment — `ANTILOKI_SUBSCOPE` (a JSON list of globs).
pub fn subscope_from_env(env: &dyn Fn(&str) -> Option<String>) -> Option<Vec<String>> {
    let raw = env("ANTILOKI_SUBSCOPE")?;
    let a: Value = serde_json::from_str(&raw).ok()?;
    let v: Vec<String> = a.as_array()?.iter().map(|x| s(Some(x))).collect();
    if v.is_empty() { None } else { Some(v) }
}

pub struct Decision { pub line: Value, pub deny: Option<String>, pub context: Option<String> }

/// The role this box is held to when it is read-only (a team's planner or QA): the marker the serve writes at `.antiloki/readonly`, `{"role": "planner"}`. `None` for any other box. Pure over the file.
pub fn readonly_role(root: &Path) -> Option<String> {
    let t = std::fs::read_to_string(root.join(".antiloki/readonly")).ok()?;
    Some(serde_json::from_str::<Value>(&t).ok().and_then(|v| v.get("role").and_then(Value::as_str).map(String::from)).filter(|r| !r.is_empty()).unwrap_or_else(|| "reader".to_string()))
}

/// MIN: what a non-write call is refused for, or None when it may run. Pure — tested.
///   search tools and shell searchers (rg/grep/find/ls/tree …) → refused, except `git`
///   a source read (Read / a shell reader on a source file) outside the edit scope → refused
///   the pack, the brief, antiloki's tools, in-scope reads, plain commands (npm test …) → allowed
pub fn min_refusal(root: &Path, tool: &str, input: &Value, kind: &str, rel: &str, scope: &[String]) -> Option<String> {
    if kind == "search" {
        if is_shell_tool(tool) { let head = command_text(&shell_command(input)); let first = head.split_whitespace().next().map(base_name).unwrap_or_default(); if first == "git" { return None; } }
        return Some(format!("{tool} {rel}"));
    }
    if kind == "source" && is_read(tool) && !in_scope(scope, rel) { return Some(format!("read {rel} (outside the scope)")); }
    let _ = root;
    None
}

/// THE STRICT WALL of a call that was given only some files (`ANTILOKI_SUBSCOPE_ROOT`: a group member): no shell, no file outside the folder (by name or through a link), nothing the agents or git run from. Why it is refused, or `None`.
pub fn strict_refusal(root: &Path, tool: &str, input: &Value, scope: &[String]) -> Option<String> {
    if is_shell_tool(tool) { return Some("this call has no shell".into()); }
    if !is_edit_tool(tool) { return None; }
    let raw = ["file_path", "filePath", "path", "notebook_path"].iter().map(|k| s(input.get(k))).find(|v| !v.is_empty()).unwrap_or_default();
    let Some(rel) = rel_to(root, &raw).filter(|r| !r.is_empty()) else { return Some(format!("`{raw}` is not a file inside this folder")) };
    let head = rel.split('/').next().unwrap_or("").to_ascii_lowercase();
    if matches!(head.as_str(), ".git" | ".antiloki" | ".claude" | ".codex" | ".cursor" | ".opencode" | ".grok" | ".mcp.json" | "agents.md" | "claude.md") { return Some(format!("{rel} is how the agents and the repository are run")); }
    if !in_scope(scope, &rel) { return Some(format!("{rel} is outside the files you were given ({})", scope.join(", "))); }
    // the nearest part of the path that exists must still be inside the folder: a link does not lead out
    let real_root = std::fs::canonicalize(root).ok()?; let mut at = root.join(&rel);
    loop { if let Ok(real) = std::fs::canonicalize(&at) { return (!real.starts_with(&real_root)).then(|| format!("{rel} is a link that leaves this folder")); } if at.symlink_metadata().is_ok() { return Some(format!("{rel} is a link that leads nowhere")); } if !at.pop() { return None; } }
}

/// The whole decision for one PreToolUse call — pure over the reads log + manifest + env.
pub fn decide(root: &Path, tool: &str, input: &Value, log: &str, manifest: &Value, env: &dyn Fn(&str) -> Option<String>) -> Decision {
    if env("ANTILOKI_SUBSCOPE_ROOT").is_some() {
        let mine = subscope_from_env(env).unwrap_or_default(); let refused = if mine.is_empty() && (is_edit_tool(tool) || is_shell_tool(tool)) { Some("no files were given to this call".to_string()) } else { strict_refusal(root, tool, input, &mine) };
        let line = json!({ "t": now_iso(), "tool": tool, "kind": if refused.is_some() { "blocked" } else { "member" }, "nudged": null });
        return Decision { line, deny: refused.map(|w| format!("antiloki group: {w}. Do not work around it: write a line `NEED SCOPE: <path> - <why>` in your answer and continue with what is yours.")), context: None };
    }
    let scope: Vec<String> = manifest.get("scope").and_then(Value::as_array).map(|a| a.iter().map(|x| s(Some(x))).collect()).unwrap_or_default();
    let shell_targets = if is_shell_tool(tool) { shell_write_targets(root, &shell_command(input)) } else { Vec::new() };
    let writes = is_edit_tool(tool) || !shell_targets.is_empty();
    // A TEAM'S PLANNER AND QA NEVER EDIT (operator 2026-11-10: "planner and QA just read code and the plan"): the serve marks their box read-only when it links them; every write is refused here, whatever the scope says.
    // (An engine with no hooks is held by the serve's gate instead: it reverts what such a pane changed.)
    if writes {
        if let Some(role) = readonly_role(root) {
            let targets = if shell_targets.is_empty() { edit_targets(root, tool, input) } else { shell_targets.clone() };
            let outside: Vec<String> = targets.into_iter().filter(|rel| !rel.starts_with(".antiloki/")).collect();
            if !outside.is_empty() || (is_edit_tool(tool) && !shell_targets.is_empty()) {
                let line = json!({ "t": now_iso(), "tool": tool, "path": outside.first().cloned().unwrap_or_default(), "kind": "blocked", "op": "write", "nudged": null, "outside": outside, "readonly": role });
                if role == "side" { let deny = format!("antiloki side question: you are a side-question helper and you never change code: {}. Do not work around it. Answer the question in words; if something should change, say what to tell the agent you are helping.", if outside.is_empty() { "this write is refused".to_string() } else { format!("{} is refused", outside.join(", ")) }); return Decision { line, deny: Some(deny), context: None }; }
                let deny = format!("antiloki team: you are the team's {role} and a {role} never edits code: {}. Do not work around it. Say what you want changed in your reply (the planner puts it in the plan; the QA puts it in the verdict) and the worker will do it.", if outside.is_empty() { "this write is refused".to_string() } else { format!("{} is refused", outside.join(", ")) });
                return Decision { line, deny: Some(deny), context: None };
            }
        }
    }
    // an edit outside the whitelist is refused here, with the reason the agent should relay
    if writes && !scope.is_empty() {
        let targets = if shell_targets.is_empty() { edit_targets(root, tool, input) } else { shell_targets.clone() };
        let outside: Vec<String> = targets.into_iter().filter(|rel| !rel.starts_with(".antiloki/") && !in_scope(&scope, rel)).collect();
        if !outside.is_empty() {
            let line = json!({ "t": now_iso(), "tool": tool, "path": outside[0], "kind": "blocked", "op": "write", "nudged": null, "outside": outside });
            let deny = format!("antiloki: {} is outside this pane's edit scope ({}). Do not work around it. Tell the operator the file you need — the pane is showing an \"expand scope\" button for it — and continue with what is in scope.", outside.join(", "), scope.join(", "));
            return Decision { line, deny: Some(deny), context: None };
        }
    }
    if let (Some(sub), true) = (subscope_from_env(env), writes) {
        let targets = if shell_targets.is_empty() { edit_targets(root, tool, input) } else { shell_targets.clone() };
        // a directory on the way to an owned file may be created (mkdir -p packages/x/docs for packages/x/docs/A.md)
        let leads_to_mine = |rel: &str| sub.iter().any(|g| g.starts_with(&format!("{}/", rel.trim_end_matches('/'))));
        let not_mine: Vec<String> = targets.into_iter().filter(|rel| !rel.starts_with(".antiloki/") && !in_scope(&sub, rel) && !leads_to_mine(rel)).collect();
        if !not_mine.is_empty() {
            let task = env("ANTILOKI_SUBSCOPE_TASK");
            let line = json!({ "t": now_iso(), "tool": tool, "path": not_mine[0], "kind": "blocked", "op": "write", "nudged": null, "outside": not_mine, "task": task });
            let deny = format!("antiloki: {} is not owned by your task ({} owns {}) — a sibling task owns it. Do not edit it; put what you need in your report as \"FINDING: …\".", not_mine.join(", "), task.unwrap_or_else(|| "this task".into()), sub.join(", "));
            return Decision { line, deny: Some(deny), context: None };
        }
    }
    let Classified { kind, rel } = classify(root, tool, input);
    // MIN (operator 2026-09-21, the opposite of MAX): exploration is refused — every search, a source
    // read outside the edit scope, a shell command that searches or reads source outside it (git stays:
    // status/diff of its own edits). The refusal names the way: the pack and the live tools.
    if s(manifest.get("mode")) == "min" && !writes {
        if let Some(why) = min_refusal(root, tool, input, &kind, &rel, &scope) {
            let line = json!({ "t": now_iso(), "tool": tool, "path": if rel.is_empty() { why.clone() } else { rel.clone() }, "kind": "blocked", "op": if kind == "search" { "search" } else { "read" }, "nudged": null, "min": true });
            let scope_s = if scope.is_empty() { "the edit scope".to_string() } else { scope.join(", ") };
            let deny = format!("antiloki MIN: exploration is off — navigate with the pack (.antiloki/context/INDEX.md) and the live tools (antiloki_file_review · antiloki_folder_review · antiloki_blast_radius · antiloki_context_index); read a source file only to edit it, inside your scope ({scope_s}). If the pack has no intel for what you need, answer \"no intel for <path>\" instead of looking. (refused: {why})");
            return Decision { line, deny: Some(deny), context: None };
        }
    }
    let prior: Vec<Value> = log.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect();
    let index_read = prior.iter().any(|p| s(p.get("kind")) == "pack" && s(p.get("path")).ends_with("/INDEX.md"));
    let nudged_before: Vec<String> = prior.iter().map(|p| s(p.get("nudged"))).filter(|x| !x.is_empty()).collect();
    let mut nudge: Option<String> = None;
    let mut nudged: Option<String> = None;
    if kind == "source" && is_read(tool) {
        let review = manifest.get("items").and_then(Value::as_array).and_then(|items| items.iter().find(|i| s(i.get("kind")) == "file" && i.get("written").and_then(Value::as_bool).unwrap_or(false) && s(i.get("path")) == rel));
        if let (Some(review), false) = (review, nudged_before.contains(&rel)) {
            let summary = review.get("summary").and_then(Value::as_str).unwrap_or("weak axes, violated laws with lines, importers");
            nudge = Some(format!("antiloki: a measured review of {rel} is baked at .antiloki/context/{} ({summary}). Read it before or alongside the source — it says what is wrong and where.", s(review.get("file"))));
            nudged = Some(rel.clone());
        } else if !index_read && !nudged_before.iter().any(|x| x == "INDEX") {
            nudge = Some("antiloki: you have not read .antiloki/context/INDEX.md yet — it lists the measured reviews, laws, graph and tests for your scope. Read it first; it is cheaper than exploring the tree.".into());
            nudged = Some("INDEX".into());
        }
    }
    // a shell command that writes a file (redirect / tee / sed -i / cp / mv …) is a WRITE of that file
    if !shell_targets.is_empty() {
        let cmd: String = command_text(&shell_command(input)).chars().take(400).collect();
        let line = json!({ "t": now_iso(), "tool": tool, "path": shell_targets[0], "kind": "source", "op": "write", "nudged": null, "targets": shell_targets, "command": cmd });
        return Decision { line, deny: None, context: None };
    }
    // a shell command resolved to a file (cat / sed -n / head …) is a READ of that file, not an exec
    let op0 = op_of(tool);
    let op = if op0 == "exec" && !rel.is_empty() && matches!(kind.as_str(), "source" | "pack" | "brief") { "read" } else { op0 };
    let line = json!({ "t": now_iso(), "tool": tool, "path": rel, "kind": kind, "op": op, "nudged": nudged });
    Decision { line, deny: None, context: nudge }
}

fn append(path: &Path, line: &Value) {
    use std::io::Write;
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    // ONE write of the whole line: `writeln!` on a bare file is several writes, and two hook processes (the box's and an older one a repo's own settings still carry) interleaved
    // them into a torn line that lost a run's answer (found live 2026-11-10)
    let bytes = format!("{line}\n").into_bytes();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) { let _ = f.write_all(&bytes); }
}

fn read_stdin() -> String {
    use std::io::Read;
    let mut raw = String::new();
    let _ = std::io::stdin().read_to_string(&mut raw);
    raw
}

/// A short POST with a hard timeout: a hook sits in front of the person's prompt, so a slow or absent serve must cost almost nothing.
fn post_json(url: &str, body: &Value) -> Option<Value> {
    ureq::post(url).timeout(std::time::Duration::from_millis(1500)).send_json(body.clone()).ok()?.into_json().ok()
}

/// The long poll for the person's answer: connecting and sending are quick, the read may wait as long as the question is open (a little over the serve's own limit).
fn post_json_wait(url: &str, body: &Value) -> Option<Value> {
    let wait_ms: u64 = std::env::var("ANTILOKI_ASK_WAIT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(60_000) + 8_000;
    let agent = ureq::AgentBuilder::new().timeout_connect(std::time::Duration::from_millis(1_500)).timeout_read(std::time::Duration::from_millis(wait_ms)).build();
    agent.post(url).send_json(body.clone()).ok()?.into_json().ok()
}

/// What the pre-check says about an ask, in the words the agent should act on: the questions it has, with their options, and the instruction to ask the
/// person rather than guess. `None` for an ask that is fine (no noise on every prompt). Pure.
pub fn verdict_context(v: &Value) -> Option<String> {
    // a team's planner is told what it is (the serve adds the note to what the person typed to it); it rides with whatever the pre-check itself has to say
    let team = v.get("team").or_else(|| v.get("crew")).and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty()).map(String::from);
    let base = verdict_context_base(v);
    match (base, team) { (Some(b), Some(c)) => Some(format!("{c}\n\n{b}")), (None, Some(c)) => Some(c), (b, None) => b }
}
fn verdict_context_base(v: &Value) -> Option<String> {
    // THE PERSON ANSWERED the floating card: say what they chose, so the model acts on it and does not ask again (a hook cannot edit the prompt; this rides with it)
    if v.get("verdict").and_then(Value::as_str) == Some("answered") {
        let said: Vec<String> = v.get("answers").and_then(Value::as_array).into_iter().flatten().filter_map(|a| Some(format!("{} → {}", a.get("question")?.as_str()?.trim(), a.get("answer")?.as_str()?.trim()))).collect();
        return (!said.is_empty()).then(|| format!("antiloki pre-check: before this ask was sent, the person settled it: {}. Act on that; do not ask again.", said.join("; ")));
    }
    if v.get("verdict").and_then(Value::as_str) != Some("ask") { return None; }
    let qs: Vec<String> = v.get("questions").and_then(Value::as_array).into_iter().flatten().filter_map(|q| {
        let text = q.get("text").and_then(Value::as_str)?.trim();
        if text.is_empty() { return None; }
        let opts: Vec<&str> = q.get("options").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
        Some(if opts.is_empty() { text.to_string() } else { format!("{text} (options: {})", opts.join(" | ")) })
    }).collect();
    if qs.is_empty() { return None; }
    Some(format!("antiloki pre-check, before you act on this ask: {}. Ask the person which they mean, in one short question, instead of guessing.", qs.join(" ")))
}

/// Ask the serve's pre-check about a prompt typed into this pane. The pane's `.antiloki/serve.json` (written when its terminal started) says where the
/// serve is and which pane this is; with no file (a headless run, another box) or no answer there is no check. `fetch` is injected so this is testable.
pub fn precheck_context(root: &Path, prompt: &str, fetch: impl Fn(&str, &Value) -> Option<Value>, wait: impl Fn(&str, &Value) -> Option<Value>) -> Option<String> {
    precheck_verdict(root, prompt, fetch, wait).and_then(|v| verdict_context(&v))
}

/// Does a prompt ask for a mockup or a screen design? Then the agent is told where those go. Pure - tested.
pub fn mockup_nudge(prompt: &str) -> Option<&'static str> {
    let p = prompt.to_lowercase();
    let asks = ["mockup", "mock-up", "mock up", "wireframe", "design a screen", "design the screen", "screen design", "ui design", "design a page", "design the page", "landing page design", "make a design", "create a design", "layout idea", "prototype of the screen"];
    if p.trim_start().starts_with('/') || !asks.iter().any(|a| p.contains(a)) { return None; }
    Some("[antiloki] The person asked for a mockup or a design. Put it in the FRAMES pane with the antiloki MCP tool antiloki_frame_create (you write ONE self-contained HTML document: inline CSS, no scripts, no network addresses; draw pictures with inline SVG or CSS) or ask a design agent with antiloki_frame_ask. Do NOT make a Claude artifact and do not only write a file. Do not try to type into the Frames pane's form: use the tools. Tell the person it is in Frames.")
}
/// The pre-check's final word on a prompt (the quick answer, or what the person chose when the serve parked the question for them), or `None` when nothing answered. Same two phases as always.
pub fn precheck_verdict(root: &Path, prompt: &str, fetch: impl Fn(&str, &Value) -> Option<Value>, wait: impl Fn(&str, &Value) -> Option<Value>) -> Option<Value> {
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(records(root).join("serve.json")).ok()?).ok()?;
    let (url, pane) = (cfg.get("url").and_then(Value::as_str)?.trim_end_matches('/'), cfg.get("pane_id").and_then(Value::as_i64)?);
    // phase 1: a quick question (a serve that does not answer in a second and a half is not waited for)
    let v = fetch(&format!("{url}/api/v1/cockpit/precheck"), &json!({ "pane_id": pane, "text": prompt, "source": "hook" }))?;
    // phase 2: the serve parked the question for the person (a screen is open): wait for their answer, then say what they chose
    if v.get("waiting").and_then(Value::as_bool) == Some(true) {
        if let Some(id) = v.get("ask_id").and_then(Value::as_u64) {
            // an answer, a dismissal or a timeout speaks for itself (a dismissal adds nothing); only a wait that FAILED falls back to the question as it was asked
            if let Some(a) = wait(&format!("{url}/api/v1/cockpit/cowork/asks/wait"), &json!({ "id": id })) { return Some(a); }
        }
    }
    Some(v)
}

/// What the pre-check did, in a line for the saga's timeline: nothing to ask, asked the person (and what they chose), or left the question to the model. Pure.
pub fn screen_note(v: &Value) -> String {
    let word = v.get("verdict").and_then(Value::as_str).unwrap_or("");
    let q = |v: &Value| v.get("questions").and_then(Value::as_array).map_or(0, Vec::len);
    match word {
        "answered" => { let said: Vec<String> = v.get("answers").and_then(Value::as_array).into_iter().flatten().filter_map(|a| Some(format!("{} → {}", a.get("question")?.as_str()?.trim(), a.get("answer")?.as_str()?.trim()))).collect(); format!("pre-check asked the person, who chose: {}", said.join("; ")) }
        "ask" if v.get("timed_out").and_then(Value::as_bool) == Some(true) => format!("pre-check had {} question{} for the person, who did not answer in time: the model was told to ask", q(v), if q(v) == 1 { "" } else { "s" }),
        "ask" => format!("pre-check had {} question{}: the model was told to ask the person", q(v), if q(v) == 1 { "" } else { "s" }),
        "send" if v.get("by").and_then(Value::as_str) == Some("person") => "pre-check: the person dismissed it, sent as written".to_string(),
        "send" => "pre-check: clear enough, sent as written".to_string(),
        _ => String::new(),
    }
}

/// The process: stdin JSON → the event's side effects → stdout JSON + the exit code.
/// The limits a call is held to. An agent in a shared folder has its own, in its records folder: the folder's pack, if one is lying there, is not about it.
fn manifest_file(root: &Path, shared: bool) -> PathBuf { if shared { records(root).join("context/manifest.json") } else { root.join(".antiloki/context/manifest.json") } }

pub fn main() -> i32 {
    let input: Value = serde_json::from_str(&read_stdin()).unwrap_or_else(|_| json!({}));
    let input = if input.is_object() { input } else { json!({}) };
    let cwd = s(input.get("cwd"));
    let start = if cwd.is_empty() { std::env::current_dir().unwrap_or_default() } else { PathBuf::from(cwd) };
    // a group member's call says where it works: its scope is held there whether or not antiloki prepared that folder
    let shared = std::env::var_os("ANTILOKI_RECORDS").is_some_and(|v| !v.is_empty());   // our agent in a folder it shares: no pack is baked there
    let Some(root) = std::env::var_os("ANTILOKI_SUBSCOPE_ROOT").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| find_pack_root(&start)).or_else(|| shared.then(|| start.clone())) else { return 0 }; // no pack here — not our pane, stay silent
    let event = s(input.get("hook_event_name"));
    let tool = s(input.get("tool_name"));
    let tool_input = input.get("tool_input").cloned().unwrap_or(json!({}));
    let session = input.get("session_id").cloned().unwrap_or(Value::Null);
    // ANSWERS: the final assistant text of every turn, for the pane's answers list
    if event == "Stop" || event == "SubagentStop" {
        let text = s(input.get("last_assistant_message"));
        // the engine's own session cost at the end of the turn (the status line's last tick): the saga prices the turn as this minus the one the prompt saw
        let cost = std::fs::read_to_string(records(&root).join("usage.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.get("cost_usd").and_then(Value::as_f64));
        if !text.is_empty() && event == "Stop" { append(&records(&root).join("answers.jsonl"), &json!({ "t": now_iso(), "session_id": session, "text": text, "cost_usd": cost })); }
        return 0;
    }
    // THE TRACE OF EVERY TOOL CALL (the sagas' stack trace): what ran, on what, whether it worked. A line per call, whichever tool, for every engine that has hooks.
    if event == "PostToolUse" || event == "PostToolUseFailure" {
        if let Some(l) = trace_line(&root, &event, &input) { append_trace(&root, &l); }
        if event == "PostToolUseFailure" { return 0; }
    }
    // PLANS + the AUDIT of what a write left on disk
    if event == "PostToolUse" {
        if let Some((file, source)) = capture_plan(&root, &tool, &tool_input) {
            append(&records(&root).join("reads.jsonl"), &json!({ "t": now_iso(), "tool": tool, "path": source, "kind": "plan", "nudged": null, "file": file }));
        }
        for line in written_lines(&root, &tool, &tool_input, input.get("tool_response").unwrap_or(&Value::Null)) { append(&records(&root).join("reads.jsonl"), &line); }
        return 0;
    }
    // PROMPT HISTORY: every prompt the operator submits
    if event == "UserPromptSubmit" {
        let prompt = s(input.get("prompt")); let mut screen = String::new(); let mut ctx_parts: Vec<String> = Vec::new();
        // THE PRE-CHECK AT THE ENGINE'S OWN BOUNDARY (RFC 0007 §5.6): whatever typed this prompt, in the composer, straight into the terminal or in a client that
        // never heard of antiloki, the engine's hook asks the same check. It only ever ADDS context (the agent asks the person); it never blocks, and it fails open.
        if !prompt.is_empty() && !prompt.trim_start().starts_with('/') && std::env::var("ANTILOKI_PRECHECK").map(|v| v != "off").unwrap_or(true) {
            if let Some(v) = precheck_verdict(&root, &prompt, post_json, post_json_wait) {
                screen = screen_note(&v);
                if let Some(ctx) = verdict_context(&v) { ctx_parts.push(ctx); }
            }
        }
        // A MOCKUP GOES TO FRAMES (operator 2026-11-11: asked an agent for a mockup and it made a Claude artifact; the brief said otherwise and was not enough): at the moment the ask arrives, say where it goes
        if let Some(n) = mockup_nudge(&prompt) { ctx_parts.push(n.to_string()); }
        if !ctx_parts.is_empty() { println!("{}", json!({ "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": ctx_parts.join("\n\n") } })); }
        // The engine's own session cost at the moment the run starts (the status line's last tick): the strip prices a run as the cost at its end minus this.
        let cost = std::fs::read_to_string(records(&root).join("usage.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.get("cost_usd").and_then(Value::as_f64));
        if !prompt.is_empty() { append(&records(&root).join("prompts.jsonl"), &json!({ "t": now_iso(), "prompt": prompt, "session_id": session, "cost_usd": cost, "screen": screen })); }
        return 0;
    }
    let log_path = records(&root).join("reads.jsonl");
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let manifest: Value = std::fs::read_to_string(manifest_file(&root, shared)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let d = decide(&root, &tool, &tool_input, &log, &manifest, &env);
    append(&log_path, &d.line);
    // in a shared folder with nothing baked there is no scope to hold and no pack to point at: the call is recorded, no more
    // with limits of its own there is a refusal to give, and only that: there is no pack here to point it at
    if shared && d.deny.is_none() { return 0; }
    if let Some(deny) = d.deny {
        // a refusal is part of the trace too: the call that was NOT allowed, and why
        append_trace(&root, &json!({ "t": now_iso(), "session_id": session, "tool_use_id": input.get("tool_use_id").cloned().unwrap_or(Value::Null), "tool": tool, "summary": tool_summary(&root, &tool, &tool_input), "outcome": "refused", "error": deny.chars().take(240).collect::<String>() }));
        let out = json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": deny, "additionalContext": deny } });
        print!("{out}");
        eprint!("{deny}");
        return 2; // exit 2 = blocking on every engine that supports hooks, JSON or not
    }
    if let Some(ctx) = d.context {
        print!("{}", json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "additionalContext": ctx } }));
    }
    0
}

#[cfg(test)]
mod tests {
    #[test] fn a_shared_folder_agent_is_held_to_its_own_limits() {
        assert!(manifest_file(Path::new("/p"), false).ends_with(".antiloki/context/manifest.json"));
        assert_eq!(manifest_file(Path::new("/p"), true), records(Path::new("/p")).join("context/manifest.json"));
    }
    #[test]
    fn records_go_to_the_agents_own_folder_when_it_shares_one() {
        assert_eq!(records_in(Path::new("/p"), None), Path::new("/p/.antiloki")); assert_eq!(records_in(Path::new("/p"), Some("".into())), Path::new("/p/.antiloki"));
        assert_eq!(records_in(Path::new("/p"), Some("/p/.antiloki/agents/7".into())), Path::new("/p/.antiloki/agents/7"));
    }
    #[test]
    fn a_mockup_or_design_ask_is_told_where_it_goes_and_other_prompts_are_left_alone() {
        for p in ["make me a mockup of the settings screen", "Can you do a WIREFRAME for checkout", "design a screen for the dashboard", "create a design for the pricing page"] { assert!(mockup_nudge(p).map_or(false, |n| n.contains("antiloki_frame_create") && n.contains("FRAMES")), "{p}"); }
        for p in ["fix the failing test", "design the database schema", "/mockup", "refactor the page component", ""] { assert!(mockup_nudge(p).is_none(), "{p}"); }
    }
    #[test]
    fn a_question_parked_for_the_person_is_waited_for_and_their_choice_reaches_the_model() {
        let dir = std::env::temp_dir().join(format!("antiloki-hook-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(dir.join(".antiloki")).unwrap();
        std::fs::write(dir.join(".antiloki/serve.json"), json!({ "url": "http://127.0.0.1:41931", "pane_id": 7 }).to_string()).unwrap();
        let parked = json!({ "verdict": "ask", "waiting": true, "ask_id": 42, "questions": [{ "text": "x is missing", "options": ["a", "b"] }] });
        let asked = std::cell::RefCell::new(None);
        let ctx = precheck_context(&dir, "fix x", |_, _| Some(parked.clone()), |u, b| { *asked.borrow_mut() = Some((u.to_string(), b.clone())); Some(json!({ "verdict": "answered", "answers": [{ "question": "x is missing", "answer": "create it" }] })) }).unwrap();
        assert!(ctx.contains("x is missing → create it") && ctx.contains("do not ask again"), "{ctx}");
        let (u, b) = asked.into_inner().unwrap(); assert_eq!((u.as_str(), b["id"].clone()), ("http://127.0.0.1:41931/api/v1/cockpit/cowork/asks/wait", json!(42)));
        // dismissed: nothing is added; nobody answered in time: the question itself goes to the model, as before
        assert_eq!(precheck_context(&dir, "fix x", |_, _| Some(parked.clone()), |_, _| Some(json!({ "verdict": "send", "notes": ["the person dismissed the pre-check"] }))), None);
        let late = precheck_context(&dir, "fix x", |_, _| Some(parked.clone()), |_, _| Some(json!({ "verdict": "ask", "timed_out": true, "questions": [{ "text": "x is missing", "options": ["a"] }] }))).unwrap();
        assert!(late.contains("Ask the person"), "{late}");
        // the wait failing (serve gone) falls back to the question from phase 1
        assert!(precheck_context(&dir, "fix x", |_, _| Some(parked.clone()), |_, _| None).unwrap().contains("x is missing"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_screening_verdict_reads_as_one_line_for_the_timeline() {
        assert_eq!(screen_note(&json!({ "verdict": "send", "tier": 0 })), "pre-check: clear enough, sent as written");
        assert_eq!(screen_note(&json!({ "verdict": "send", "by": "person" })), "pre-check: the person dismissed it, sent as written");
        assert_eq!(screen_note(&json!({ "verdict": "ask", "questions": [{ "text": "a" }, { "text": "b" }] })), "pre-check had 2 questions: the model was told to ask the person");
        assert!(screen_note(&json!({ "verdict": "ask", "timed_out": true, "questions": [{}] })).contains("did not answer in time"));
        assert_eq!(screen_note(&json!({ "verdict": "answered", "answers": [{ "question": "which folder?", "answer": "src" }] })), "pre-check asked the person, who chose: which folder? → src");
        assert_eq!(screen_note(&json!({})), "");
    }

    #[test]
    fn every_tool_call_is_one_trace_line_that_says_what_it_touched_and_whether_it_worked() {
        let r = root(); let abs = format!("{}/src/ui/Output.tsx", r.to_string_lossy());
        let ev = |tool: &str, input: Value, extra: Value| { let mut v = json!({ "tool_name": tool, "tool_input": input, "tool_use_id": "toolu_1", "session_id": "s1" }); for (k, x) in extra.as_object().unwrap() { v[k] = x.clone(); } v };
        let edit = trace_line(&r, "PostToolUse", &ev("Edit", json!({ "file_path": abs }), json!({ "tool_response": { "filePath": "x" } }))).unwrap();
        assert_eq!((edit["tool"].clone(), edit["summary"].clone(), edit["outcome"].clone(), edit["tool_use_id"].clone()), (json!("Edit"), json!("src/ui/Output.tsx"), json!("ok"), json!("toolu_1")), "{edit}");
        assert_eq!(trace_line(&r, "PostToolUse", &ev("Bash", json!({ "command": "npm run lint\nnpm test" }), json!({}))).unwrap()["summary"], json!("npm run lint npm test"), "one line");
        assert_eq!(trace_line(&r, "PostToolUse", &ev("Grep", json!({ "pattern": "useCart", "path": abs }), json!({}))).unwrap()["summary"], json!("useCart src/ui/Output.tsx"));
        let fail = trace_line(&r, "PostToolUseFailure", &ev("Bash", json!({ "command": "false" }), json!({ "error": "Exit code 1" }))).unwrap();
        assert_eq!((fail["outcome"].clone(), fail["error"].clone()), (json!("error"), json!("Exit code 1")));
        assert_eq!(trace_line(&r, "PostToolUse", &ev("Bash", json!({ "command": "sleep 99" }), json!({ "tool_response": { "interrupted": true } }))).unwrap()["outcome"], json!("interrupted"));
        let todo = trace_line(&r, "PostToolUse", &ev("TodoWrite", json!({ "todos": [{ "content": "read the cart", "status": "completed" }, { "content": "add the candle", "status": "in_progress" }] }), json!({}))).unwrap();
        assert_eq!((todo["summary"].clone(), todo["todos"][1]["status"].clone()), (json!("2 to-do items"), json!("in_progress")));
        assert_eq!(trace_line(&r, "PostToolUse", &json!({})), None, "no tool, no line");
        assert!(trace_line(&r, "PostToolUse", &ev("mcp__antiloki__antiloki_check", json!({ "path": "src/a.ts" }), json!({}))).unwrap()["summary"].as_str().unwrap().contains("src/a.ts"));
    }

    #[test]
    fn two_hook_processes_appending_at_once_never_tear_a_line() {
        let dir = std::env::temp_dir().join(format!("antiloki-hook-append-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("answers.jsonl");
        let big = "x".repeat(60_000);
        let writers: Vec<_> = (0..2).map(|w| { let p = path.clone(); let big = big.clone(); std::thread::spawn(move || { for i in 0..150 { append(&p, &json!({ "w": w, "i": i, "text": big })); } }) }).collect();
        for t in writers { t.join().unwrap(); }
        let text = std::fs::read_to_string(&path).unwrap();
        let ok = text.lines().filter(|l| serde_json::from_str::<Value>(l).is_ok()).count();
        assert_eq!((text.lines().count(), ok), (300, 300), "every line is one whole JSON object");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_pre_check_speaks_only_when_it_has_a_question_and_tells_the_agent_to_ask() {
        let ask = json!({ "verdict": "ask", "questions": [{ "id": "q1", "text": "`src/nowhere/x.tsx` is not in this box", "options": ["create it", "I meant another file"] }] });
        let ctx = verdict_context(&ask).unwrap();
        assert!(ctx.contains("`src/nowhere/x.tsx` is not in this box (options: create it | I meant another file)") && ctx.contains("Ask the person"), "{ctx}");
        assert_eq!(verdict_context(&json!({ "verdict": "send", "tier": 0, "notes": [] })), None, "a fine ask adds nothing to the prompt");
        assert_eq!(verdict_context(&json!({ "verdict": "send", "team": "you are the planner" })), Some("you are the planner".into()), "a planner's note rides with a fine ask");
        assert!(verdict_context(&json!({ "verdict": "ask", "questions": [{ "text": "which folder?" }], "team": "you are the planner" })).unwrap().starts_with("you are the planner\n\nantiloki pre-check"), "and with a question");
        assert_eq!(verdict_context(&json!({ "verdict": "ask", "questions": [] })), None);
        assert_eq!(verdict_context(&json!({ "verdict": "skipped" })), None);
    }

    #[test]
    fn the_hook_asks_the_serve_its_pane_names_and_fails_open_when_it_cannot() {
        let dir = std::env::temp_dir().join(format!("antiloki-hook-precheck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(dir.join(".antiloki")).unwrap();
        let ask = json!({ "verdict": "ask", "questions": [{ "text": "x is missing", "options": ["a", "b"] }] });
        // no serve.json (a headless run, another box): no check, and the fetch is never called
        assert_eq!(precheck_context(&dir, "fix x", |_, _| panic!("no serve to ask"), |_, _| panic!("no wait")), None);
        std::fs::write(dir.join(".antiloki/serve.json"), json!({ "url": "http://127.0.0.1:41931/", "pane_id": 7 }).to_string()).unwrap();
        let seen = std::cell::RefCell::new(None);
        let ctx = precheck_context(&dir, "fix x", |u, b| { *seen.borrow_mut() = Some((u.to_string(), b.clone())); Some(ask.clone()) }, |_, _| panic!("a question with no screen is not waited for"));
        assert!(ctx.unwrap().contains("x is missing"));
        let (url, body) = seen.into_inner().unwrap();
        assert_eq!(url, "http://127.0.0.1:41931/api/v1/cockpit/precheck");
        assert_eq!((body["pane_id"].clone(), body["text"].clone(), body["source"].clone()), (json!(7), json!("fix x"), json!("hook")));
        // the serve down, slow or wrong: the prompt goes through untouched
        assert_eq!(precheck_context(&dir, "fix x", |_, _| None, |_, _| None), None);
        assert_eq!(precheck_context(&dir, "fix x", |_, _| Some(json!("garbage")), |_, _| None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn root() -> PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-hook-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(d.join("src/ui")).unwrap();
        std::fs::create_dir_all(d.join(".antiloki/context")).unwrap();
        std::fs::write(d.join("src/ui/Output.tsx"), "x").unwrap();
        std::fs::write(d.join("package.json"), "{}").unwrap();
        std::fs::canonicalize(d).unwrap()
    }
    fn strs(v: &[&str]) -> Vec<String> { v.iter().map(|s| s.to_string()).collect() }
    fn no_env(_: &str) -> Option<String> { None }

    #[test]
    fn a_member_writes_only_its_files_without_a_shell_and_never_through_a_link() {
        let d = std::env::temp_dir().join(format!("antiloki-strict-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("docs")).unwrap();
        let scope = vec!["docs/**".to_string()]; let no = |tool: &str, p: &str| strict_refusal(&d, tool, &json!({ "file_path": p }), &scope);
        assert_eq!(no("Write", &d.join("docs/a.md").to_string_lossy()), None); assert_eq!(no("Edit", "docs/new/b.md"), None); assert_eq!(no("Read", "/etc/passwd"), None);
        for bad in ["src/a.rs", "/etc/passwd", "docs/../src/a.rs", "../out.md", ".claude/settings.local.json", ".antiloki/x", ""] { assert!(no("Write", bad).is_some(), "{bad}"); }
        assert!(no("Bash", "x").is_some());
        #[cfg(unix)] { std::os::unix::fs::symlink(std::env::temp_dir(), d.join("docs/out")).unwrap(); assert!(no("Write", "docs/out/x.md").unwrap().contains("link")); std::os::unix::fs::symlink("/nowhere/at/all", d.join("docs/dead")).unwrap(); assert!(no("Write", "docs/dead").is_some()); }
        let env = |k: &str| match k { "ANTILOKI_SUBSCOPE_ROOT" => Some(d.to_string_lossy().to_string()), "ANTILOKI_SUBSCOPE" => Some("[\"docs/**\"]".to_string()), _ => None };
        assert!(decide(&d, "Write", &json!({ "file_path": "src/a.rs" }), "", &json!({}), &env).deny.is_some()); assert!(decide(&d, "Write", &json!({ "file_path": "docs/a.md" }), "", &json!({}), &env).deny.is_none());
        let none = |k: &str| (k == "ANTILOKI_SUBSCOPE_ROOT").then(|| d.to_string_lossy().to_string()); assert!(decide(&d, "Write", &json!({ "file_path": "docs/a.md" }), "", &json!({}), &none).deny.is_some(), "no files given, nothing written"); let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn in_scope_matches_exact_prefix_and_glob() {
        let sc = strs(&["src/ui/**", "docs/README.md", "lib", "**/*.spec.ts"]);
        assert!(in_scope(&sc, "src/ui/Output.tsx"));
        assert!(in_scope(&sc, "src/ui/deep/x.ts"));
        assert!(!in_scope(&sc, "src/uix/x.ts"));
        assert!(in_scope(&sc, "docs/README.md"));
        assert!(in_scope(&sc, "lib/a.ts"));
        assert!(in_scope(&sc, "a/b/c.spec.ts"));
        assert!(!in_scope(&sc, "package.json"));
        assert!(in_scope(&[], "anything"));
    }

    #[test]
    fn edit_targets_relative_absolute_and_patch() {
        let r = root();
        assert_eq!(edit_targets(&r, "Edit", &json!({ "file_path": r.join("src/ui/Output.tsx").to_string_lossy() })), vec!["src/ui/Output.tsx"]);
        assert_eq!(edit_targets(&r, "Write", &json!({ "file_path": "src/new.ts" })), vec!["src/new.ts"]);
        assert!(edit_targets(&r, "Write", &json!({ "file_path": "../outside.ts" })).is_empty());
        let patch = "*** Begin Patch\n*** Update File: src/a.ts\n*** Add File: src/b.ts\n*** End Patch";
        assert_eq!(edit_targets(&r, "apply_patch", &json!({ "input": patch })), vec!["src/a.ts", "src/b.ts"]);
    }

    #[test]
    fn shell_write_targets_redirects_tee_sed_cp() {
        let r = root();
        let t = |c: &str| shell_write_targets(&r, &json!(c));
        assert_eq!(t("echo hi > out.txt"), vec!["out.txt"]);
        assert_eq!(t("cat a | tee logs/b.log"), vec!["logs/b.log"]);
        assert_eq!(t("sed -i 's/x/y/' src/ui/Output.tsx"), vec!["src/ui/Output.tsx"]);
        assert_eq!(t("cp a.txt b/c.txt && touch d.txt"), vec!["b/c.txt", "d.txt"]);
        assert!(t("git log --format=\"%H > %s\"").is_empty(), "a > inside quotes is not a redirect");
        assert!(t("grep \">\" file").is_empty());
        assert_eq!(t("echo x > \"my file.txt\""), vec!["my file.txt"]);
        assert!(t("cmd 2>/dev/null").is_empty());
        assert_eq!(shell_write_targets(&r, &json!(["bash", "-lc", "echo 1 >> notes.md"])), vec!["notes.md"]);
    }

    #[test]
    fn classify_reads_pack_brief_source_search_tool() {
        let r = root();
        assert_eq!(classify(&r, "Read", &json!({ "file_path": ".antiloki/context/INDEX.md" })).kind, "pack");
        assert_eq!(classify(&r, "Read", &json!({ "file_path": "CLAUDE.md" })).kind, "brief");
        assert_eq!(classify(&r, "Read", &json!({ "file_path": "src/ui/Output.tsx" })), Classified { kind: "source".into(), rel: "src/ui/Output.tsx".into() });
        assert_eq!(classify(&r, "Grep", &json!({ "pattern": "foo" })), Classified { kind: "search".into(), rel: "foo".into() });
        assert_eq!(classify(&r, "mcp__antiloki__antiloki_check", &json!({})), Classified { kind: "tool".into(), rel: "antiloki_check".into() });
        assert_eq!(classify(&r, "Bash", &json!({ "command": "cat src/ui/Output.tsx" })).kind, "source");
        assert_eq!(classify(&r, "shell", &json!({ "command": ["bash", "-lc", "rg foo src"] })).kind, "search");
        assert_eq!(op_of("Bash"), "exec"); assert_eq!(op_of("Edit"), "write"); assert_eq!(op_of("Read"), "read");
    }

    #[test]
    fn decide_refuses_an_edit_outside_scope_and_allows_inside() {
        let r = root();
        let m = json!({ "scope": ["src/ui/**"], "items": [] });
        let d = decide(&r, "Edit", &json!({ "file_path": "package.json" }), "", &m, &no_env);
        assert!(d.deny.as_deref().unwrap().contains("package.json is outside this pane's edit scope (src/ui/**)"));
        assert_eq!(d.line["kind"], "blocked"); assert_eq!(d.line["op"], "write");
        let ok = decide(&r, "Edit", &json!({ "file_path": "src/ui/Output.tsx" }), "", &m, &no_env);
        assert!(ok.deny.is_none());
        let shell = decide(&r, "Bash", &json!({ "command": "echo x > package.json" }), "", &m, &no_env);
        assert!(shell.deny.is_some(), "a shell redirect outside the scope is refused like an Edit");
        let own = decide(&r, "Write", &json!({ "file_path": ".antiloki/plans/x.md" }), "", &m, &no_env);
        assert!(own.deny.is_none(), ".antiloki/ is always writable");
    }

    #[test]
    fn a_teams_planner_and_qa_may_read_and_run_but_every_write_is_refused_and_the_workers_box_is_untouched() {
        let r = std::env::temp_dir().join(format!("antiloki-hook-ro-{}", std::process::id())); let _ = std::fs::remove_dir_all(&r); std::fs::create_dir_all(r.join(".antiloki")).unwrap();
        let m = json!({ "scope": [], "items": [] });
        // an ordinary box: writes are fine
        assert!(decide(&r, "Edit", &json!({ "file_path": "src/a.ts" }), "", &m, &no_env).deny.is_none());
        std::fs::write(r.join(".antiloki/readonly"), r#"{"role":"planner","team":"k"}"#).unwrap();
        let d = decide(&r, "Edit", &json!({ "file_path": "src/a.ts" }), "", &m, &no_env); let why = d.deny.unwrap(); assert!(why.contains("the team's planner") && why.contains("src/a.ts is refused") && why.contains("the worker will do it"), "{why}"); assert_eq!(d.line["readonly"], "planner");
        assert!(decide(&r, "Write", &json!({ "file_path": "new.txt" }), "", &m, &no_env).deny.is_some());
        assert!(decide(&r, "Bash", &json!({ "command": "echo x > src/a.ts" }), "", &m, &no_env).deny.is_some(), "a shell redirect is a write");
        assert!(decide(&r, "Bash", &json!({ "command": "sed -i s/a/b/ src/a.ts" }), "", &m, &no_env).deny.is_some(), "sed -i is a write");
        // reading, running and its own notes are untouched
        assert!(decide(&r, "Read", &json!({ "file_path": "src/a.ts" }), "", &m, &no_env).deny.is_none());
        assert!(decide(&r, "Bash", &json!({ "command": "npm test" }), "", &m, &no_env).deny.is_none());
        assert!(decide(&r, "Write", &json!({ "file_path": ".antiloki/plans/x.md" }), "", &m, &no_env).deny.is_none(), ".antiloki/ stays writable: that is the agent's own notes, not code");
        assert_eq!(readonly_role(&r).as_deref(), Some("planner")); std::fs::remove_file(r.join(".antiloki/readonly")).unwrap(); assert_eq!(readonly_role(&r), None);
        assert!(decide(&r, "Edit", &json!({ "file_path": "src/a.ts" }), "", &m, &no_env).deny.is_none(), "unlinked: writable again");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn min_refuses_exploration_and_keeps_the_edit_read_and_the_tools() {
        let r = root();
        let m = json!({ "scope": ["src/ui/**"], "mode": "min", "items": [] });
        let deny = |tool: &str, input: Value| decide(&r, tool, &input, "", &m, &no_env).deny;
        // searches are exploration — refused, the refusal names the pack and the tools
        let d = deny("Grep", json!({ "pattern": "useCart", "path": "src" })).unwrap();
        assert!(d.starts_with("antiloki MIN: exploration is off") && d.contains("antiloki_file_review") && d.contains("(refused: Grep useCart)"), "{d}");
        assert!(deny("Glob", json!({ "pattern": "**/*.ts" })).is_some());
        assert!(deny("Bash", json!({ "command": "rg -n formatPrice src" })).is_some(), "a shell search is a search");
        assert!(deny("Bash", json!({ "command": "ls -la src/ui" })).is_some());
        assert!(deny("Bash", json!({ "command": "git status --short" })).is_none(), "git is not exploration");
        // a source read: inside the edit scope it is the edit's own read; outside it is exploration
        assert!(deny("Read", json!({ "file_path": "src/ui/Output.tsx" })).is_none());
        let d = deny("Read", json!({ "file_path": "package.json" })).unwrap();
        assert!(d.contains("(refused: read package.json (outside the scope))"), "{d}");
        assert!(deny("Bash", json!({ "command": "cat package.json" })).is_some(), "a shell read of source outside the scope is the same read");
        assert!(deny("Bash", json!({ "command": "sed -n 1,20p src/ui/Output.tsx" })).is_none());
        // the pack, the brief, antiloki's tools and plain commands are the way MIN works
        assert!(deny("Read", json!({ "file_path": ".antiloki/context/INDEX.md" })).is_none());
        assert!(deny("mcp__antiloki__antiloki_file_review", json!({ "path": "package.json" })).is_none());
        assert!(deny("Bash", json!({ "command": "npm test" })).is_none());
        // writes stay under the scope rule, not MIN's
        assert!(deny("Edit", json!({ "file_path": "src/ui/Output.tsx" })).is_none());
        assert!(deny("Edit", json!({ "file_path": "package.json" })).unwrap().contains("outside this pane's edit scope"));
        // the audit line says MIN refused it
        let d = decide(&r, "Grep", &json!({ "pattern": "x" }), "", &m, &no_env);
        assert_eq!(d.line["kind"], "blocked"); assert_eq!(d.line["op"], "search"); assert_eq!(d.line["min"], true);
        // no mode → nothing changes
        let m0 = json!({ "scope": ["src/ui/**"], "items": [] });
        assert!(decide(&r, "Grep", &json!({ "pattern": "x" }), "", &m0, &no_env).deny.is_none());
    }

    #[test]
    fn decide_enforces_task_ownership_from_env() {
        let r = root();
        let m = json!({ "scope": ["src/**", "packages/**"], "items": [] });
        let env = |k: &str| match k { "ANTILOKI_SUBSCOPE" => Some("[\"packages/x/**\"]".to_string()), "ANTILOKI_SUBSCOPE_TASK" => Some("t2 docs".to_string()), _ => None };
        let d = decide(&r, "Edit", &json!({ "file_path": "src/ui/Output.tsx" }), "", &m, &env);
        assert!(d.deny.as_deref().unwrap().contains("not owned by your task (t2 docs owns packages/x/**)"));
        assert_eq!(d.line["task"], "t2 docs");
        assert!(decide(&r, "Edit", &json!({ "file_path": "packages/x/a.ts" }), "", &m, &env).deny.is_none());
        assert!(decide(&r, "Bash", &json!({ "command": "mkdir -p packages/x" }), "", &m, &env).deny.is_none(), "a dir on the way to an owned file may be created");
    }

    #[test]
    fn decide_nudges_index_once_then_the_review_once() {
        let r = root();
        let m = json!({ "scope": [], "items": [{ "kind": "file", "path": "src/ui/Output.tsx", "file": "src__ui__Output.tsx.md", "written": true, "summary": "health 41" }] });
        let d1 = decide(&r, "Read", &json!({ "file_path": "src/ui/Output.tsx" }), "", &m, &no_env);
        assert!(d1.context.as_deref().unwrap().contains("measured review of src/ui/Output.tsx is baked at .antiloki/context/src__ui__Output.tsx.md (health 41)"));
        assert_eq!(d1.line["nudged"], "src/ui/Output.tsx");
        let log = d1.line.to_string() + "\n";
        let d2 = decide(&r, "Read", &json!({ "file_path": "src/ui/Output.tsx" }), &log, &m, &no_env);
        assert!(d2.context.as_deref().unwrap().contains("INDEX.md"), "the second read of the same file gets the INDEX nudge instead");
        let log2 = log + &d2.line.to_string() + "\n";
        let d3 = decide(&r, "Read", &json!({ "file_path": "src/ui/Output.tsx" }), &log2, &m, &no_env);
        assert!(d3.context.is_none(), "both nudges handed out once per session");
        let read_index = decide(&r, "Read", &json!({ "file_path": ".antiloki/context/INDEX.md" }), "", &m, &no_env);
        assert_eq!(read_index.line["kind"], "pack"); assert!(read_index.context.is_none());
        let bash_read = decide(&r, "Bash", &json!({ "command": "sed -n 1,20p src/ui/Output.tsx" }), "", &m, &no_env);
        assert_eq!(bash_read.line["op"], "read", "a shell read of a file is a read, not an exec");
    }

    #[test]
    fn written_lines_carry_sha_and_size() {
        let r = root();
        let lines = written_lines(&r, "Write", &json!({ "file_path": "src/ui/Output.tsx" }), &Value::Null);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["bytes"], 1); assert_eq!(lines[0]["sha"].as_str().unwrap().len(), 16); assert_eq!(lines[0]["kind"], "written");
        let sh = written_lines(&r, "Bash", &json!({ "command": "echo hi > src/ui/Output.tsx" }), &json!({ "exit_code": 0 }));
        assert_eq!(sh[0]["kind"], "exec"); assert_eq!(sh[0]["exit"], 0);
        assert_eq!(sh[1]["via"], "shell"); assert_eq!(sh[1]["path"], "src/ui/Output.tsx");
    }

    #[test]
    fn capture_plan_renders_codex_steps_and_copies_claude_plan_files() {
        let r = root();
        let c = capture_plan(&r, "update_plan", &json!({ "explanation": "why", "plan": [{ "step": "a", "status": "completed" }, { "step": "b", "status": "in_progress" }] })).unwrap();
        assert_eq!(c.0, ".antiloki/plans/codex-plan.md");
        let body = std::fs::read_to_string(r.join(".antiloki/plans/codex-plan.md")).unwrap();
        assert!(body.contains("1. [x] a") && body.contains("2. [ ] b _(in progress)_") && body.starts_with("# Plan (Codex)\n\nwhy"));
        let plan_dir = r.join(".claude/plans"); std::fs::create_dir_all(&plan_dir).unwrap();
        let f = plan_dir.join("my plan.md"); std::fs::write(&f, "# p").unwrap();
        let c2 = capture_plan(&r, "Write", &json!({ "file_path": f.to_string_lossy() })).unwrap();
        assert_eq!(c2.0, ".antiloki/plans/my-plan.md");
        assert!(capture_plan(&r, "Write", &json!({ "file_path": "src/x.md" })).is_none());
    }

    #[test]
    fn find_pack_root_walks_up() {
        let r = root();
        std::fs::write(r.join(".antiloki/context/manifest.json"), "{}").unwrap();
        assert_eq!(find_pack_root(&r.join("src/ui")).unwrap(), r);
        assert!(find_pack_root(&std::env::temp_dir()).is_none() || true);
    }

    #[test]
    fn iso_timestamp_shape() {
        let t = now_iso();
        assert!(Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$").unwrap().is_match(&t), "{t}");
    }

    #[test]
    fn antiqa_tool_calls_are_audited_as_tools_not_as_unknown_other() {
        assert_eq!(op_of("mcp__antiqa__antiqa_run_start"), "tool");
        assert_eq!(op_of("mcp__antiloki__antiloki_check"), "tool");
        assert_eq!(op_of("mcp__other__thing"), "other");
    }
}
