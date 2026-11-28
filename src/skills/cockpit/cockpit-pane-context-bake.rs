//! cockpit-pane-context-bake.rs — materialise a pane's CONTEXT PACK into its worktree. NO JS TWIN.
//! NAMES ARE CONTRACT: `cockpitPaneContextBake` (default), action `system.cockpit.pane.context.bake`.
//!
//! Added 2026-09-14. Takes the selection `context-pack.rs` resolves for the pane and writes it to
//! `<worktree>/.antiloki/context/`:
//!
//! ```text
//! INDEX.md                 what is here, the reading rule, the default-pack rule, staleness
//! laws.md                  the ratified constitution (review-markdown::laws_md)
//! folders/<slug>.md        folder reviews          files/<slug>.md    file reviews
//! graph/<slug>.json        runtime import graphs   tests/<slug>.md    test maps
//! blast/<slug>.md          blast radii             docs/<id>.md       blackboard context docs (live sources embedded)
//! manifest.json            {hash, engineering_version, constitution_hash, snapshot_id, head_sha, generated, items[]}
//! ```
//!
//! Then it (1) writes `.antiloki/mcp.json` when the pane has live tools, (2) patches `opencode.json`
//! (`instructions` + `mcp`) for proxied engines, (3) appends `.antiloki/` to the worktree's git
//! `info/exclude` so a pack can never reach a PR, (4) records the pack hash on the pane's
//! `settings_json` so the next bake with nothing changed is a no-op, and (5) rewrites the PANE
//! BRIEF block in AGENTS.md / CLAUDE.md so the block lists exactly what was written.
//!
//! Budget: items are rendered in order and written until `budget_chars` is hit; the rest are
//! listed in INDEX.md and the brief as "not materialised" (never silently dropped — the same rule
//! as smart-context-bundle). The bake never judges: a path without a snapshot gets an honest stub.
//!
//! Atomic enough: everything is written to `.antiloki/context.tmp-<pid>` and renamed into place,
//! so an agent reading mid-bake sees the old pack or the new one, never a half of either.
//!
//! BESPOKE-NATIVE: real fs writes + a best-effort `git` subprocess; proven with native tests on a
//! temp worktree, not conform-db fixtures.
/// Seconds the engine lets the prompt hook run: it waits up to 60 s for the person to answer the pre-check's question (`cowork_asks`), plus the requests either side.
pub const PROMPT_HOOK_TIMEOUT_S: u64 = 75;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn s(v: Option<&Value>) -> String { match v { Some(Value::String(x)) => x.clone(), Some(Value::Null) | None => String::new(), Some(o) => o.to_string() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn int(v: Option<&Value>) -> String { nz(v).and_then(Value::as_f64).map(|f| format!("{}", f.round() as i64)).unwrap_or_else(|| "—".into()) }
fn take(sv: &str, n: usize) -> String { sv.chars().take(n).collect() }

pub const CONTEXT_DIR: &str = ".antiloki/context";
pub const MCP_CONFIG: &str = ".antiloki/mcp.json";
/// THE PANE KINDS THAT GET A CONTEXT PACK AND A BRIEF — every seat (audit 2026-09-28). While this was four,
/// a Gemini, Grok or Cursor pane was baked nothing: no pack, no scope line, no AGENTS.md/CLAUDE.md brief. The
/// agent ran with none of the context every other seat is given. Keep in step with `serve::chat_pane::ENGINES`.
pub const CODE_AGENT_KINDS: &[&str] = &["opencode", "opencode-direct", "claude-code", "codex", "gemini", "grok", "cursor"];

fn git_out(args: &[&str], cwd: &Path) -> Option<String> {
    let o = crate::proc::cmd("git").args(args).current_dir(cwd).output().ok()?;
    if !o.status.success() { return None; }
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// THE SIDECAR (2026-09-18, "make it Rust all the way"): `antiloki-agent` — one binary, three
/// subcommands (`hook` · `mcp` · `statusline`) — replaces the three Node scripts the BE used to
/// ship. Resolved once per bake, in this order:
///   1. `ANTILOKI_AGENT_BIN` (the shell / a dev BE pins it)
///   2. next to the running executable — where the Tauri bundle places its externalBin
///   3. the cargo target dir beside this crate (dev: `cargo build -p antiloki-agent`)
///   4. bare `antiloki-agent`, resolved by PATH at spawn time
pub const AGENT_BIN: &str = "antiloki-agent";
pub fn agent_bin() -> String {
    if let Ok(v) = std::env::var("ANTILOKI_AGENT_BIN") { if !v.is_empty() { return v; } }
    let exe_name = if cfg!(windows) { "antiloki-agent.exe" } else { "antiloki-agent" };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let next = dir.join(exe_name);
            if next.exists() { return next.to_string_lossy().into_owned(); }
        }
    }
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    for profile in ["debug", "release"] {
        let p = target.join(profile).join(exe_name);
        if p.exists() { return p.to_string_lossy().into_owned(); }
    }
    AGENT_BIN.to_string()
}
fn agent_cmd(sub: &str) -> String {
    let bin = agent_bin();
    let quoted = if bin.contains(' ') { format!("\"{bin}\"") } else { bin };
    format!("{quoted} {sub}")
}
/// Both the new binary and the old scripts count as ours when an existing config is merged.
pub const HOOK_MARK: &str = "antiloki-agent";
const OLD_HOOK_MARK: &str = "antiloki-hook";
fn ours(entry: &Value) -> bool { let s = entry.to_string(); s.contains(HOOK_MARK) || s.contains(OLD_HOOK_MARK) }

/// `.claude/settings.local.json` (Claude Code's project-local layer) learns ONE PreToolUse hook —
/// the nudge-and-measure script — merged into whatever the file already holds; our entry is
/// recognised by its command and replaced, never duplicated. The repo's own `.claude/settings.json`
/// is untouched.
fn write_claude_hook(wt: &Path) { claude_hook_into(&wt.join(".claude").join("settings.local.json")) }
/// For an agent that shares its folder: the same hooks and status line in a settings file of its own, given to Claude Code on its command line (`--settings`), so nothing is written into the project's `.claude`.
/// Its own tool settings sit beside it, naming its pane.
pub fn write_shared_agent_settings(records: &Path, pane_id: i64, folder: &str, live: bool) {
    claude_hook_into(&records.join("settings.json"));
    let file = records.join("mcp.json");
    if live { let _ = std::fs::write(file, serde_json::to_string_pretty(&json!({ "mcpServers": mcp_servers(pane_id, folder, None) })).unwrap_or_default()); } else { let _ = std::fs::remove_file(file); }
}
fn claude_hook_into(file: &Path) {
    if let Some(dir) = file.parent() { let _ = std::fs::create_dir_all(dir); }
    let mut cfg: Value = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    if !cfg.is_object() { cfg = json!({}); }
    let hooks = cfg.as_object_mut().unwrap().entry("hooks").or_insert(json!({}));
    if !hooks.is_object() { *hooks = json!({}); }
    let pre = hooks.as_object_mut().unwrap().entry("PreToolUse").or_insert(json!([]));
    if !pre.is_array() { *pre = json!([]); }
    let arr = pre.as_array_mut().unwrap();
    arr.retain(|e| !ours(e));
    // reads + searches (nudge, measurement) and every write tool (scope enforced before the write)
    arr.push(json!({
        // Bash too: `cat`/`sed` reads are classified from argv, so a shell read counts like a Read
        "matcher": "Read|Grep|Glob|Bash|Edit|Write|MultiEdit|NotebookEdit|mcp__antiloki__.*|mcp__antiqa__.*",
        "hooks": [{ "type": "command", "command": agent_cmd("hook"), "timeout": 10 }],
    }));
    // PROMPT HISTORY · ANSWERS · PLANS: the prompt on submit, the final text on Stop, a plan file after it is written
    // PostToolUse for EVERY tool (the trace of what the model did, for the sagas) and its failure event; the hook decides what else to do with a given tool
    for (event, matcher) in [("UserPromptSubmit", None), ("Stop", None), ("PostToolUse", Some(".*")), ("PostToolUseFailure", Some(".*"))] {
        let ev = hooks.as_object_mut().unwrap().entry(event).or_insert(json!([]));
        if !ev.is_array() { *ev = json!([]); }
        let ev_arr = ev.as_array_mut().unwrap();
        ev_arr.retain(|e| !ours(e));
        // the prompt hook may WAIT for the person to answer the pre-check's question (the floating card): its own limit, the others stay at ten seconds
        let mut entry = json!({ "hooks": [{ "type": "command", "command": agent_cmd("hook"), "timeout": if event == "UserPromptSubmit" { PROMPT_HOOK_TIMEOUT_S } else { 10 } }] });
        if let Some(m) = matcher { entry["matcher"] = json!(m); }
        ev_arr.push(entry);
    }
    // USAGE ALWAYS VISIBLE: the status line prints context · cost · 5h · week inside the terminal and
    // writes .antiloki/usage.json for the pane chip; a refresh timer keeps the windows current while idle.
    cfg.as_object_mut().unwrap().insert("statusLine".into(), json!({ "type": "command", "command": agent_cmd("statusline"), "refreshInterval": 60 }));
    let _ = std::fs::write(file, serde_json::to_string_pretty(&cfg).unwrap_or_default());
}

/// 2026-10-13: in a main checkout (`.git` is a directory), a tracked project file is hands-off — linked worktrees (`.git` is a file) keep the rewrite behaviour.
fn hands_off(wt: &Path, rel: &str) -> bool { wt.join(".git").is_dir() && git_out(&["ls-files", "--error-unmatch", rel], wt).is_some() }

/// `.cursor/mcp.json` - `{"mcpServers": {"antiloki": {command, args, env}}}`, merged into whatever the project already has there and removed again when the pane has no live tools.
fn write_cursor_mcp(wt: &Path, pane_id: i64, live: bool, antiqa: Option<&Value>) {
    if hands_off(wt, ".cursor/mcp.json") { return; }
    let file = wt.join(".cursor").join("mcp.json");
    let mut cfg: Value = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    if !cfg.is_object() { cfg = json!({}); }
    let servers = cfg.as_object_mut().unwrap().entry("mcpServers").or_insert(json!({}));
    if !servers.is_object() { *servers = json!({}); }
    if live { servers.as_object_mut().unwrap().insert("antiloki".into(), mcp_server_entry(pane_id, &wt.to_string_lossy())); } else { servers.as_object_mut().unwrap().remove("antiloki"); }
    // antiQA beside it while the button is on; off, only OUR entry leaves (the project's own servers stay)
    match antiqa.filter(|_| live) { Some(a) => { servers.as_object_mut().unwrap().insert("antiqa".into(), a.clone()); } None => { servers.as_object_mut().unwrap().remove("antiqa"); } }
    let empty = servers.as_object().map(|o| o.is_empty()).unwrap_or(true) && cfg.as_object().map(|o| o.len() == 1).unwrap_or(false);
    if empty { let _ = std::fs::remove_file(&file); return; }
    let _ = std::fs::create_dir_all(wt.join(".cursor"));
    let _ = std::fs::write(&file, serde_json::to_string_pretty(&cfg).unwrap_or_default());
}

const GROK_BEGIN: &str = "# >>> antiloki (generated, replaced on every bake)";
const GROK_END: &str = "# <<< antiloki";
/// The TOML block for Grok's project config: `[mcp_servers.antiloki]` with its env table. JSON string escapes are valid TOML basic strings. Pure - tested.
pub fn grok_mcp_block(pane_id: i64, worktree: &str) -> String { grok_mcp_block_with(pane_id, worktree, None) }
/// The same block with `[mcp_servers.antiqa]` after antiloki's when the pane has antiQA switched on.
pub fn grok_mcp_block_with(pane_id: i64, worktree: &str, antiqa: Option<&Value>) -> String {
    let q = |v: &Value| serde_json::to_string(v.as_str().unwrap_or("")).unwrap_or_else(|_| "\"\"".into());
    let table = |name: &str, e: &Value| -> String {
        let args = e["args"].as_array().map(|a| a.iter().map(&q).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        let mut t = format!("[mcp_servers.{name}]\ncommand = {}\nargs = [{args}]\nenabled = true\n\n[mcp_servers.{name}.env]\n", q(&e["command"]));
        if let Some(env) = e["env"].as_object() { for (k, v) in env { t.push_str(&format!("{k} = {}\n", q(v))); } }
        t
    };
    let mut out = format!("{GROK_BEGIN}\n{}", table("antiloki", &mcp_server_entry(pane_id, worktree)));
    if let Some(a) = antiqa { out.push('\n'); out.push_str(&table("antiqa", a)); }
    out.push_str(GROK_END); out.push('\n');
    out
}
/// Our block out of a config text (the text without it), leaving the project's own lines alone. Pure - tested.
pub fn without_grok_block(text: &str) -> String {
    let (Some(i), Some(j)) = (text.find(GROK_BEGIN), text.find(GROK_END)) else { return text.to_string() };
    if j < i { return text.to_string(); }
    let tail = &text[j + GROK_END.len()..];
    format!("{}{}", &text[..i], tail.strip_prefix('\n').unwrap_or(tail)).trim_end().to_string()
}
/// `.grok/config.toml` - the antiloki MCP server as a marked block, appended to (or replaced in) the project's own file.
fn write_grok_mcp(wt: &Path, pane_id: i64, live: bool, antiqa: Option<&Value>) {
    if hands_off(wt, ".grok/config.toml") { return; }
    let file = wt.join(".grok").join("config.toml");
    let have = std::fs::read_to_string(&file).unwrap_or_default();
    let rest = without_grok_block(&have);
    if !live {
        if rest.trim().is_empty() { let _ = std::fs::remove_file(&file); } else if rest != have { let _ = std::fs::write(&file, format!("{rest}\n")); }
        return;
    }
    let _ = std::fs::create_dir_all(wt.join(".grok"));
    let sep = if rest.trim().is_empty() { String::new() } else { format!("{rest}\n\n") };
    let _ = std::fs::write(&file, format!("{sep}{}", grok_mcp_block_with(pane_id, &wt.to_string_lossy(), antiqa)));
}

/// `.codex/hooks.json` — Codex reads the SAME hook schema Claude Code does (event names, stdin,
/// stdout), so the one script serves both. Merged like the Claude file, replaced never duplicated.
fn write_codex_hook(wt: &Path) {
    let dir = wt.join(".codex");
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("hooks.json");
    let mut cfg: Value = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
    if !cfg.is_object() { cfg = json!({}); }
    let hooks = cfg.as_object_mut().unwrap().entry("hooks").or_insert(json!({}));
    if !hooks.is_object() { *hooks = json!({}); }
    let pre = hooks.as_object_mut().unwrap().entry("PreToolUse").or_insert(json!([]));
    if !pre.is_array() { *pre = json!([]); }
    let arr = pre.as_array_mut().unwrap();
    arr.retain(|e| !ours(e));
    // Codex reads through its shell tool; the script parses argv for the paths
    arr.push(json!({
        "matcher": ".*",
        "hooks": [{ "type": "command", "command": agent_cmd("hook"), "timeout": 10 }],
    }));
    for (event, matcher) in [("UserPromptSubmit", None), ("Stop", None), ("PostToolUse", Some(".*"))] {
        let ev = hooks.as_object_mut().unwrap().entry(event).or_insert(json!([]));
        if !ev.is_array() { *ev = json!([]); }
        let ev_arr = ev.as_array_mut().unwrap();
        ev_arr.retain(|e| !ours(e));
        // the prompt hook may WAIT for the person to answer the pre-check's question (the floating card): its own limit, the others stay at ten seconds
        let mut entry = json!({ "hooks": [{ "type": "command", "command": agent_cmd("hook"), "timeout": if event == "UserPromptSubmit" { PROMPT_HOOK_TIMEOUT_S } else { 10 } }] });
        if let Some(m) = matcher { entry["matcher"] = json!(m); }
        ev_arr.push(entry);
    }
    let _ = std::fs::write(&file, serde_json::to_string_pretty(&cfg).unwrap_or_default());
}

/// `.opencode/plugins/antiloki-context.js` — OpenCode has no command hooks but auto-loads project
/// plugins; this one does the same two jobs in-process: log every tool call to `.antiloki/reads.jsonl`
/// and prepend the review nudge to the result of a `read` on a source file with a baked review.
pub const OPENCODE_PLUGIN_JS: &str = r#"// antiloki-context — written by antiloki's context bake; replaced on every bake, never edit.
// Two jobs: (1) log every tool call to .antiloki/reads.jsonl (pack vs source vs search vs tool),
// (2) when the agent reads a source file that has a baked review, prepend a one-line pointer to the
// review onto the tool result — once per file — and once per session point at INDEX.md if the first
// source read happens before the index was opened. Never blocks.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const hooksFor = (root) => {
  const ctxDir = path.join(root, ".antiloki", "context");
  const logPath = path.join(root, ".antiloki", "reads.jsonl");
  const manifest = () => { try { return JSON.parse(fs.readFileSync(path.join(ctxDir, "manifest.json"), "utf8")); } catch { return {}; } };
  const prior = () => { try { return fs.readFileSync(logPath, "utf8").split("\n").filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean); } catch { return []; } };
  const rel = (p) => path.relative(root, path.isAbsolute(p) ? p : path.join(root, p)).replace(/\\/g, "/");
  const kindOf = (tool, args) => {
    if (/antiloki/i.test(tool)) return { kind: "tool", rel: tool };
    if (tool === "grep" || tool === "glob" || tool === "list") return { kind: "search", rel: String(args.pattern ?? args.path ?? "") };
    const raw = args.filePath ?? args.path ?? "";
    if (!raw) return { kind: "other", rel: "" };
    const r = rel(String(raw));
    if (r.startsWith("..")) return { kind: "other", rel: r };
    if (r.startsWith(".antiloki/context/")) return { kind: "pack", rel: r };
    if (/^(\.antiloki|\.opencode|\.agents|\.claude|\.codex)\//.test(r) || r === "AGENTS.md" || r === "CLAUDE.md") return { kind: "brief", rel: r };
    return { kind: "source", rel: r };
  };
  const inScope = (scope, r) => !scope.length || scope.some((g) => g === r || (g.endsWith("/**") && (r + "/").startsWith(g.slice(0, -2))) || (!/[*?[]/.test(g) && r.startsWith(g.replace(/\/$/, "") + "/")));
  // SHELL WRITES (audit): redirect / tee / sed -i / cp / mv / rm / touch targets inside the box —
  // treated like an edit target: scope-gated, logged as a write, hashed afterwards.
  const shellWrites = (cmd) => {
    const flat = String(cmd ?? ""); const out = new Set();
    const push = (raw) => { const s = raw.replace(/^['"]|['"]$/g, ""); if (!s || s === "/dev/null" || s.startsWith("-") || s.startsWith("&")) return; const r = rel(s); if (r && !r.startsWith("..")) out.add(r); };
    // quoted strings are consumed whole: a `>` inside them is text, not a redirect
    const Q = `"(?:[^"\\\\]|\\\\.)*"|'[^']*'`;
    for (const m of flat.matchAll(new RegExp(`${Q}|(?:^|[^<>&|\\d])\\d?>{1,2}\\s*(${Q}|[^\\s|;&<>]+)`, "g"))) if (m[1]) push(m[1]);
    for (const seg of flat.replace(new RegExp(Q, "g"), '""').split(/\s*(?:\|\||&&|;|\|)\s*/)) {
      const toks = seg.trim().split(/\s+/).filter(Boolean); if (!toks.length) continue;
      const base = path.basename(toks[0].replace(/^['"]|['"]$/g, "")); const args = toks.slice(1); const ops = args.filter((a) => !a.startsWith("-"));
      if (base === "tee") ops.forEach(push);
      else if (base === "sed" && args.some((a) => /^-[a-zA-Z]*i/.test(a))) ops.slice(1).forEach(push);
      else if (base === "cp" || base === "mv") { if (ops.length >= 2) push(ops[ops.length - 1]); }
      else if (["touch", "rm", "mkdir", "rmdir", "truncate"].includes(base)) ops.forEach(push);
    }
    return Array.from(out);
  };
  const shaOf = (r) => { try { const buf = fs.readFileSync(path.join(root, r)); return { sha: crypto.createHash("sha256").update(buf).digest("hex").slice(0, 16), bytes: buf.length }; } catch { return { sha: null, bytes: null }; } };
  const pending = new Map();
  return {
    // PROMPT HISTORY: what the operator typed, for the pane's prompt window
    "chat.message": async (input, output) => {
      try {
        const parts = Array.isArray(output?.parts) ? output.parts : [];
        const text = parts.filter((p) => p && p.type === "text" && typeof p.text === "string").map((p) => p.text).join("\n").trim();
        if (text) fs.appendFileSync(path.join(root, ".antiloki", "prompts.jsonl"), JSON.stringify({ t: new Date().toISOString(), prompt: text, session_id: input?.sessionID ?? null }) + "\n");
      } catch {}
    },
    "tool.execute.before": async (input, output) => {
      if (!fs.existsSync(path.join(ctxDir, "manifest.json"))) return;
      // the edit whitelist, enforced before the write — the refusal names the file and tells the agent to ask
      const shellTargets = input.tool === "bash" ? shellWrites(output.args?.command ?? "") : [];
      if (/^(edit|write|patch|multiedit)$/.test(String(input.tool)) || shellTargets.length) {
        const scope = manifest().scope ?? [];
        const target = output.args?.filePath ?? output.args?.path ?? "";
        // a patch names its files inside its text: every one of them is held to the limits
        const patched = Array.from(String(output.args?.patchText ?? output.args?.patch ?? "").matchAll(/^\*\*\* (?:(?:Add|Update|Delete) File|Move to): (.+)$/gm), (m) => rel(m[1].trim()));
        const targets = shellTargets.length ? shellTargets : patched.length ? patched : [target ? rel(String(target)) : ""].filter(Boolean);
        const outside = targets.filter((r0) => !r0.startsWith(".antiloki/") && scope.length && !inScope(scope, r0));
        if (outside.length) {
          try { fs.appendFileSync(logPath, JSON.stringify({ t: new Date().toISOString(), tool: input.tool, path: outside[0], kind: "blocked", op: "write", nudged: null, outside }) + "\n"); } catch {}
          throw new Error(`antiloki: ${outside.join(", ")} is outside this pane's edit scope (${scope.join(", ")}). Do not work around it. Tell the operator the file you need — the pane is showing an "expand scope" button for it — and continue with what is in scope.`);
        }
        // GOAL RUNS: a task's ownership rides on the environment — a write to a sibling's file is refused
        let sub = null; try { const a = JSON.parse(process.env.ANTILOKI_SUBSCOPE || "null"); sub = Array.isArray(a) && a.length ? a.map(String) : null; } catch {}
        const notMine = sub ? targets.filter((r0) => !r0.startsWith(".antiloki/") && !inScope(sub, r0)) : [];
        if (notMine.length) {
          try { fs.appendFileSync(logPath, JSON.stringify({ t: new Date().toISOString(), tool: input.tool, path: notMine[0], kind: "blocked", op: "write", nudged: null, outside: notMine, task: process.env.ANTILOKI_SUBSCOPE_TASK ?? null }) + "\n"); } catch {}
          throw new Error(`antiloki: ${notMine.join(", ")} is not owned by your task (${process.env.ANTILOKI_SUBSCOPE_TASK ?? "this task"} owns ${sub.join(", ")}) — a sibling task owns it. Do not edit it; put what you need in your report as "FINDING: …".`);
        }
        if (shellTargets.length) {
          try { fs.appendFileSync(logPath, JSON.stringify({ t: new Date().toISOString(), tool: input.tool, path: shellTargets[0], kind: "source", op: "write", nudged: null, targets: shellTargets, command: String(output.args?.command ?? "").slice(0, 400) }) + "\n"); } catch {}
          return;
        }
      }
      const { kind, rel: r } = kindOf(String(input.tool), output.args ?? {});
      // MIN (2026-09-21): exploration is refused — searches, source reads outside the edit scope, and shell
      // commands that search or read source (git stays: status/diff of its own edits). The refusal names the way.
      if (manifest().mode === "min") {
        const scope = manifest().scope ?? [];
        const explain = `antiloki MIN: exploration is off — navigate with the pack (.antiloki/context/INDEX.md) and the live tools (antiloki_file_review · antiloki_folder_review · antiloki_blast_radius · antiloki_context_index); read a source file only to edit it, inside your scope (${scope.join(", ") || "the edit scope"}). If the pack has no intel for what you need, answer "no intel for <path>" instead of looking.`;
        let why = "";
        if (kind === "search") why = `${input.tool} ${r}`;
        else if (kind === "source" && input.tool === "read" && !inScope(scope, r)) why = `read ${r} (outside the scope)`;
        else if (input.tool === "bash") {
          const cmd = String(output.args?.command ?? "");
          const toks = cmd.split(/[\s|;&()<>]+/).filter(Boolean).map((t) => t.replace(/^['"]|['"]$/g, ""));
          const bases = toks.map((t) => path.basename(t));
          const searcher = bases.some((b) => ["rg", "grep", "egrep", "fgrep", "ag", "find", "fd", "ls", "tree"].includes(b));
          const reader = bases.some((b) => ["cat", "head", "tail", "sed", "awk", "less", "more", "bat", "nl"].includes(b));
          const files = toks.filter((t) => !t.startsWith("-") && t.length >= 2).map((t) => rel(t)).filter((x) => x && !x.startsWith("..") && fs.existsSync(path.join(root, x)) && fs.statSync(path.join(root, x)).isFile());
          const srcOutside = files.filter((x) => !/^(\.antiloki|\.opencode|\.agents|\.claude|\.codex)\//.test(x) && x !== "AGENTS.md" && x !== "CLAUDE.md" && !inScope(scope, x));
          if (searcher) why = `shell search: ${cmd.slice(0, 80)}`;
          else if (reader && srcOutside.length) why = `shell read of ${srcOutside[0]} (outside the scope)`;
        }
        if (why) {
          try { fs.appendFileSync(logPath, JSON.stringify({ t: new Date().toISOString(), tool: input.tool, path: r || why, kind: "blocked", op: kind === "search" ? "search" : "read", nudged: null, min: true }) + "\n"); } catch {}
          throw new Error(`${explain} (refused: ${why})`);
        }
      }
      const p = prior();
      const indexRead = p.some((x) => x.kind === "pack" && /\/INDEX\.md$/.test(String(x.path)));
      const nudgedBefore = new Set(p.map((x) => x.nudged).filter(Boolean).map(String));
      let nudge = ""; let nudged = null;
      if (kind === "source" && input.tool === "read") {
        const review = (manifest().items ?? []).find((i) => i.kind === "file" && i.written && i.path === r);
        if (review && !nudgedBefore.has(r)) { nudge = `antiloki: a measured review of ${r} is baked at .antiloki/context/${review.file} (${review.summary ?? "weak axes, violated laws with lines, importers"}). Read it before or alongside the source — it says what is wrong and where.`; nudged = r; }
        else if (!indexRead && !nudgedBefore.has("INDEX")) { nudge = "antiloki: you have not read .antiloki/context/INDEX.md yet — it lists the measured reviews, laws, graph and tests for your scope. Read it first; it is cheaper than exploring the tree."; nudged = "INDEX"; }
      }
      const op = /^(edit|write|patch|multiedit)$/.test(String(input.tool)) ? "write" : input.tool === "read" ? "read" : kind === "search" ? "search" : input.tool === "bash" ? "exec" : kind === "tool" ? "tool" : "other";
      try { fs.appendFileSync(logPath, JSON.stringify({ t: new Date().toISOString(), tool: input.tool, path: r, kind, op, nudged }) + "\n"); } catch {}
      if (nudge) pending.set(input.callID, nudge);
    },
    "tool.execute.after": async (input, output) => {
      // AUDIT: the outcome of a write (sha + size on disk now) and every shell command that ran
      try {
        if (/^(edit|write|patch|multiedit)$/.test(String(input.tool))) {
          const target = output?.args?.filePath ?? output?.args?.path ?? input?.args?.filePath ?? input?.args?.path ?? "";
          if (target) {
            const r = rel(String(target));
            fs.appendFileSync(logPath, JSON.stringify({ t: new Date().toISOString(), tool: input.tool, path: r, kind: "written", op: "write", nudged: null, ...shaOf(r) }) + "\n");
          }
        } else if (input.tool === "bash") {
          const cmd = String(output?.args?.command ?? input?.args?.command ?? "").slice(0, 400);
          if (cmd) fs.appendFileSync(logPath, JSON.stringify({ t: new Date().toISOString(), tool: input.tool, path: cmd, kind: "exec", op: "exec", nudged: null, exit: output?.metadata?.exit ?? null }) + "\n");
          for (const r of shellWrites(cmd)) fs.appendFileSync(logPath, JSON.stringify({ t: new Date().toISOString(), tool: input.tool, path: r, kind: "written", op: "write", nudged: null, ...shaOf(r), via: "shell" }) + "\n");
        }
      } catch {}
      // PLANS: OpenCode's todo list is its plan — rendered to markdown for the accept/reject artefact
      if (input.tool === "todowrite" || input.tool === "todo_write") {
        try {
          const todos = Array.isArray(output?.args?.todos) ? output.args.todos : Array.isArray(input?.args?.todos) ? input.args.todos : [];
          if (todos.length) {
            const dir = path.join(root, ".antiloki", "plans");
            fs.mkdirSync(dir, { recursive: true });
            fs.writeFileSync(path.join(dir, "opencode-todo.md"), `# Plan (OpenCode)\n\n${todos.map((td, i) => `${i + 1}. [${td?.status === "completed" ? "x" : " "}] ${String(td?.content ?? td?.step ?? "")}${td?.status === "in_progress" ? " _(in progress)_" : ""}`).join("\n")}\n`);
            fs.appendFileSync(logPath, JSON.stringify({ t: new Date().toISOString(), tool: input.tool, path: "todowrite", kind: "plan", nudged: null, file: ".antiloki/plans/opencode-todo.md" }) + "\n");
          }
        } catch {}
      }
      const nudge = pending.get(input.callID);
      if (!nudge) return;
      pending.delete(input.callID);
      if (typeof output.output === "string") output.output = `${nudge}\n\n${output.output}`;
    },
  };
};

// OPENCODE 2 takes a default export with an id and a setup, and names the shell `shell` and a call's arguments `input` (read from a probe on 2.4.2, 2026-11-28: throwing in `execute.before` refuses the call).
// This is only the doorway: every rule is in `hooksFor`.
const nameOf = (t) => (t === "shell" ? "bash" : t === "apply_patch" ? "patch" : String(t ?? ""));
export default {
  id: "antiloki-context",
  setup: async (ctx) => {
    const hooks = hooksFor(ctx.location?.directory || process.cwd());
    await ctx.session.hook("prompt", (e) => hooks["chat.message"]({ sessionID: e.sessionID }, { parts: [{ type: "text", text: e.prompt?.text ?? "" }] }));
    await ctx.tool.hook("execute.before", (e) => hooks["tool.execute.before"]({ tool: nameOf(e.tool), callID: e.id }, { args: e.input ?? {} }));
    await ctx.tool.hook("execute.after", async (e) => {
      const first = e.result?.content?.[0];
      const output = { args: e.input ?? {}, metadata: { exit: e.result?.output?.exitCode ?? null }, output: first?.type === "text" ? first.text : undefined };
      await hooks["tool.execute.after"]({ tool: nameOf(e.tool), callID: e.id, args: e.input ?? {} }, output);
      if (first?.type === "text" && output.output !== first.text) first.text = output.output;
    });
  },
};
"#;

fn write_opencode_plugin(wt: &Path) {
    let dir = wt.join(".opencode").join("plugins");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("antiloki-context.js"), OPENCODE_PLUGIN_JS);
}

/// `.antiloki/reads.jsonl` (written by the hook) → the measurement: reads from the pack vs from
/// source, searches, live tool calls, nudges handed out, and the last few lines.
pub fn reads_summary(wt: &Path) -> Value {
    let text = std::fs::read_to_string(wt.join(".antiloki").join("reads.jsonl")).unwrap_or_default();
    let mut pack = 0i64; let mut source = 0i64; let mut search = 0i64; let mut tool = 0i64; let mut brief = 0i64; let mut other = 0i64; let mut nudged = 0i64;
    let mut written = 0i64; let mut exec = 0i64;
    let mut index_read = false;
    let mut last: Vec<Value> = Vec::new();
    // out-of-scope writes the hook refused — newest first, one entry per path, for the pane's "expand scope" banner
    let mut blocked: Vec<Value> = Vec::new();
    let mut plan_events: Vec<Value> = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        match v.get("kind").and_then(Value::as_str).unwrap_or("") {
            "pack" => { pack += 1; if v.get("path").and_then(Value::as_str).map_or(false, |p| p.ends_with("/INDEX.md")) { index_read = true; } }
            "source" => source += 1,
            "search" => search += 1,
            "tool" => tool += 1,
            "brief" => brief += 1,
            "blocked" => {
                let p = s(v.get("path"));
                blocked.retain(|b| s(b.get("path")) != p);
                blocked.insert(0, json!({ "path": p, "tool": v.get("tool").cloned().unwrap_or(Value::Null), "t": v.get("t").cloned().unwrap_or(Value::Null) }));
                blocked.truncate(10);
            }
            "plan" => plan_events.push(v.clone()),
            "written" => written += 1,
            "exec" => exec += 1,
            _ => other += 1,
        }
        if !v.get("nudged").map_or(true, Value::is_null) { nudged += 1; }
        last.push(v);
    }
    let n = last.len();
    let tail: Vec<Value> = last.into_iter().skip(n.saturating_sub(12)).collect();
    json!({ "total": n, "pack": pack, "source": source, "search": search, "tool": tool, "brief": brief, "other": other, "written": written, "exec": exec, "nudged": nudged, "index_read": index_read, "blocked": blocked, "plans": plans_summary(wt, &plan_events), "last": tail })
}

/// AUDIT (operator 2026-09-16: "each file that an agent reads or writes — how to ensure
/// auditability?"). Three layers, all in one answer:
///   1. the HOOK LOG — every tool call the engine made (`op` read/write/search/exec/tool), the
///      refused writes, and the `written` outcome lines (sha + size on disk after each write);
///   2. GIT — what the box actually holds that base does not (uncommitted + committed-ahead files),
///      which is the ground truth for writes no matter how they happened (a shell redirect included);
///   3. the CROSS-CHECK — files git says changed that the hook never saw written (`git_only`: a
///      write that bypassed the tool layer — a script, a redirect, a generator) and hook writes git
///      does not show (`hook_only`: reverted, or ignored). Plus the log's own sha256 so a copy of the
///      audit can be verified later against the box.
pub fn audit_summary(wt: &Path, base: &str, op_filter: &str, path_filter: &str, limit: usize) -> Value {
    use sha2::Digest;
    let text = std::fs::read_to_string(wt.join(".antiloki").join("reads.jsonl")).unwrap_or_default();
    let log_sha = hex::encode(sha2::Sha256::digest(text.as_bytes()));
    let mut events: Vec<Value> = Vec::new();
    // per path: reads · writes · blocked · last sha
    let mut files: Map<String, Value> = Map::new();
    let mut first_t = String::new(); let mut last_t = String::new();
    let (mut reads, mut writes, mut blocked, mut execs, mut searches, mut tools) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
    for line in text.lines() {
        let Ok(mut v) = serde_json::from_str::<Value>(line) else { continue };
        let kind = s(v.get("kind"));
        let tool = s(v.get("tool"));
        // older lines carry no `op` — derive it the same way the hook does now
        let is_file_kind = kind == "source" || kind == "pack" || kind == "brief";
        let mut op = if !s(v.get("op")).is_empty() { s(v.get("op")) } else if kind == "written" || kind == "blocked" { "write".into() } else if kind == "exec" { "exec".into() } else if kind == "search" { "search".into() } else if kind == "tool" { "tool".into() } else if kind == "plan" { "plan".into() } else if tool == "Read" || tool == "read_file" || tool == "read" { "read".into() } else if tool == "Bash" || tool == "shell" || tool == "local_shell" { "exec".into() } else { "other".into() };
        // a shell command the hook resolved to a file (cat / sed -n / head …) is a READ of that file
        if op == "exec" && is_file_kind && !s(v.get("path")).is_empty() { op = "read".into(); }
        if let Some(o) = v.as_object_mut() { o.insert("op".into(), json!(op)); }
        let t = s(v.get("t"));
        if first_t.is_empty() { first_t = t.clone(); }
        last_t = t.clone();
        let path = s(v.get("path"));
        match (op.as_str(), kind.as_str()) {
            ("read", _) if kind == "source" || kind == "pack" || kind == "brief" || kind == "other" => reads += 1,
            ("write", "written") => writes += 1,
            ("write", "blocked") => blocked += 1,
            ("exec", _) => execs += 1,
            ("search", _) => searches += 1,
            ("tool", _) => tools += 1,
            _ => {}
        }
        if !path.is_empty() && (kind == "source" || kind == "pack" || kind == "brief" || kind == "written" || kind == "blocked") {
            let e = files.entry(path.clone()).or_insert_with(|| json!({ "path": path, "reads": 0, "writes": 0, "blocked": 0, "first": t, "last": t, "sha": Value::Null, "bytes": Value::Null }));
            let o = e.as_object_mut().unwrap();
            let bump = |o: &mut Map<String, Value>, k: &str| { let n = o.get(k).and_then(Value::as_i64).unwrap_or(0); o.insert(k.into(), json!(n + 1)); };
            match kind.as_str() {
                "written" => { bump(o, "writes"); o.insert("sha".into(), v.get("sha").cloned().unwrap_or(Value::Null)); o.insert("bytes".into(), v.get("bytes").cloned().unwrap_or(Value::Null)); }
                "blocked" => bump(o, "blocked"),
                _ => if op == "read" { bump(o, "reads") },
            }
            o.insert("last".into(), json!(t));
        }
        events.push(v);
    }
    // git ground truth: what the box holds that base does not (antiloki-owned files excluded)
    let ours = |f: &str| f.starts_with(".antiloki/") || f.starts_with(".claude/") || f.starts_with(".codex/") || f.starts_with(".opencode/") || f.starts_with(".agents/") || f == "AGENTS.md" || f == "CLAUDE.md" || f == "opencode.json";
    // NOT git_out: it trims the output and the first porcelain line's status column goes with it
    let porcelain = crate::proc::cmd("git").args(["status", "--porcelain", "--untracked-files=all"]).current_dir(wt).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut git_changed: Vec<String> = porcelain.lines().filter(|l| l.len() > 3).map(|l| l[3..].trim().trim_matches('"').to_string()).map(|f| f.rsplit(" -> ").next().unwrap_or("").to_string()).filter(|f| !f.is_empty() && !ours(f)).collect();
    if !base.is_empty() {
        for f in git_out(&["diff", "--name-only", &format!("{base}...HEAD")], wt).unwrap_or_default().lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && !ours(l)) {
            if !git_changed.contains(&f) { git_changed.push(f); }
        }
    }
    git_changed.sort();
    let hook_written: Vec<String> = { let mut v: Vec<String> = files.values().filter(|f| f.get("writes").and_then(Value::as_i64).unwrap_or(0) > 0).map(|f| s(f.get("path"))).collect(); v.sort(); v };
    let git_only: Vec<String> = git_changed.iter().filter(|f| !hook_written.contains(f)).cloned().collect();
    let hook_only: Vec<String> = hook_written.iter().filter(|f| !git_changed.contains(f)).cloned().collect();
    let matched: Vec<String> = hook_written.iter().filter(|f| git_changed.contains(f)).cloned().collect();
    // the event list, newest first, filtered
    let mut filtered: Vec<Value> = events.into_iter().rev()
        .filter(|e| op_filter.is_empty() || s(e.get("op")) == op_filter)
        .filter(|e| path_filter.is_empty() || s(e.get("path")).contains(path_filter))
        .collect();
    let event_total = filtered.len();
    filtered.truncate(limit.max(1));
    let mut file_rows: Vec<Value> = files.into_values().collect();
    file_rows.sort_by(|a, b| s(b.get("last")).cmp(&s(a.get("last"))));
    json!({
        "log": { "sha256": log_sha, "lines": text.lines().count(), "first": first_t, "last": last_t, "file": ".antiloki/reads.jsonl" },
        "counts": { "reads": reads, "writes": writes, "blocked": blocked, "exec": execs, "search": searches, "tool": tools },
        "files": file_rows,
        "git": { "base": base, "changed": git_changed },
        "cross": { "matched": matched, "git_only": git_only, "hook_only": hook_only },
        "events": filtered, "event_total": event_total,
    })
}

pub fn audit_md(a: &Value) -> String {
    let c = a.get("counts").cloned().unwrap_or(json!({}));
    let cross = a.get("cross").cloned().unwrap_or(json!({}));
    let list = |k: &str| cross.get(k).and_then(Value::as_array).map(|v| v.iter().map(|x| s(Some(x))).collect::<Vec<_>>()).unwrap_or_default();
    let mut md = format!("# Audit

reads {} · writes {} · refused {} · shell {} · searches {} · live tools {}
log {} lines · sha256 {} · {} → {}
",
        int(c.get("reads")), int(c.get("writes")), int(c.get("blocked")), int(c.get("exec")), int(c.get("search")), int(c.get("tool")),
        int(a.get("log").and_then(|l| l.get("lines"))), s(a.get("log").and_then(|l| l.get("sha256"))).chars().take(16).collect::<String>(), s(a.get("log").and_then(|l| l.get("first"))), s(a.get("log").and_then(|l| l.get("last"))));
    md.push_str("
## Writes vs git

");
    let m = list("matched"); let g = list("git_only"); let h = list("hook_only");
    md.push_str(&format!("- matched (hook saw the write, git shows the change): {}
", if m.is_empty() { "none".into() } else { m.join(", ") }));
    md.push_str(&format!("- git only (changed in the box, NEVER seen as a tool write — a script, a redirect, a generator): {}
", if g.is_empty() { "none".into() } else { g.join(", ") }));
    md.push_str(&format!("- hook only (written by a tool, not in git now — reverted or ignored): {}
", if h.is_empty() { "none".into() } else { h.join(", ") }));
    md.push_str("
## Files

| path | reads | writes | refused | last sha | last |
|---|---:|---:|---:|---|---|
");
    for f in a.get("files").and_then(Value::as_array).into_iter().flatten().take(200) {
        md.push_str(&format!("| {} | {} | {} | {} | {} | {} |
", s(f.get("path")), int(f.get("reads")), int(f.get("writes")), int(f.get("blocked")), s(f.get("sha")), s(f.get("last"))));
    }
    md
}

/// The plans in `.antiloki/plans/` with their decision (from `decisions.jsonl`), newest first.
pub fn plans_summary(wt: &Path, events: &[Value]) -> Vec<Value> {
    let dir = wt.join(".antiloki").join("plans");
    let decisions: Vec<Value> = std::fs::read_to_string(dir.join("decisions.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect();
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<(std::time::SystemTime, Value)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if !name.ends_with(".md") { continue; }
        let body = std::fs::read_to_string(&p).unwrap_or_default();
        let title = body.lines().find(|l| l.starts_with('#')).map(|l| l.trim_start_matches('#').trim().to_string()).unwrap_or_else(|| name.trim_end_matches(".md").to_string());
        let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        let file = format!(".antiloki/plans/{name}");
        let decided = decisions.iter().rev().find(|d| s(d.get("file")) == file).cloned();
        let written_t = events.iter().rev().find(|ev| s(ev.get("file")) == file).map(|ev| ev.get("t").cloned().unwrap_or(Value::Null)).unwrap_or(Value::Null);
        // a decision older than the plan's last write is stale — the plan was revised since
        let status = match &decided {
            Some(d) if s(d.get("t")) >= s(Some(&written_t)) => s(d.get("decision")),
            _ => "pending".to_string(),
        };
        out.push((modified, json!({ "file": file, "title": title, "chars": body.chars().count(), "t": written_t, "status": status, "reason": decided.as_ref().and_then(|d| d.get("reason").cloned()).unwrap_or(Value::Null), "decided_at": decided.as_ref().and_then(|d| d.get("t").cloned()).unwrap_or(Value::Null) })));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, v)| v).collect()
}

fn be_url() -> String {
    let port = std::env::var("ANTILOKI_BE_PORT").or_else(|_| std::env::var("PORT")).unwrap_or_else(|_| "41931".to_string());
    format!("http://127.0.0.1:{port}")
}

/// The MCP server entry every engine config points at (Claude `.antiloki/mcp.json`, Codex `-c`
/// overrides built by the BE, OpenCode `opencode.json.mcp`).
pub fn mcp_server_entry(pane_id: i64, worktree: &str) -> Value {
    json!({
        "command": agent_bin(),
        "args": ["mcp"],
        "env": { "ANTILOKI_BE_URL": be_url(), "ANTILOKI_PANE_ID": pane_id.to_string(), "ANTILOKI_PANE_WORKTREE": worktree },
    })
}

/// ANTIQA IS A SECOND MCP SERVER IN THE PANE, NEVER A MODEL (RFC 0001 §7.2, operator 2026-10-26). `settings.antiqa.enabled` is the button in
/// the Heimdall menu; `antiqa` is the external, headless QA tool (`antiqa mcp`) that plays user stories against the running app. It tests
/// the features the agent changed, not the project. None when the button is off, or when the binary cannot be found (`ANTIQA_BIN`, else PATH):
/// a config entry for a program that is not there would only make the CLI report a dead server.
pub fn antiqa_server_entry(pane: &Value, pane_id: i64, worktree: &str) -> Option<Value> {
    let settings: Value = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    if settings["antiqa"]["enabled"] != json!(true) { return None; }
    let bin = antiqa_bin_from(std::env::var("ANTIQA_BIN").ok(), || crate::proc::which("antiqa"))?;
    let wt = Path::new(worktree);
    // the project's own stories live with the project; the pane's box is where it is allowed to start checkouts from
    let packs = settings["antiqa"]["packs"].as_str().filter(|p| !p.trim().is_empty()).map(String::from).unwrap_or_else(|| wt.join(".antiqa").join("packs").to_string_lossy().into_owned());
    Some(json!({
        "command": bin,
        "args": ["mcp", "--out", wt.join(".antiloki").join("qa-reports").to_string_lossy(), "--allow-root", worktree],
        // ANTIQA_ORIGIN names the caller in antiQA's run records and audit log; antiQA refuses to start runs for an `antiqa:` caller
        "env": { "ANTIQA_ORIGIN": format!("antiloki:pane-{pane_id}"), "QAA_PACKS_DIR": packs },
    }))
}

/// Which `antiqa` to run: `ANTIQA_BIN` if set, else the one on PATH, else none. Pure - tested (the lookup is passed in, so a test never touches PATH).
pub fn antiqa_bin_from(env: Option<String>, on_path: impl FnOnce() -> Option<PathBuf>) -> Option<String> {
    env.filter(|v| !v.trim().is_empty()).or_else(|| on_path().map(|p| p.to_string_lossy().into_owned()))
}

/// The `mcpServers` object every JSON engine config carries: antiloki always, antiQA when the pane asked for it and it is installed.
pub fn mcp_servers(pane_id: i64, worktree: &str, antiqa: Option<&Value>) -> Value {
    let mut m = Map::new();
    m.insert("antiloki".into(), mcp_server_entry(pane_id, worktree));
    if let Some(a) = antiqa { m.insert("antiqa".into(), a.clone()); }
    // THE WEB APPS INSTALLED ON THIS MACHINE that have an MCP server: the agent's own CLI connects to them (it asks for its own sign-in)
    for (k, v) in crate::cockpit__helpers_webapps::servers_of(&crate::cockpit__helpers_webapps::load()) { m.entry(k).or_insert(v); }
    Value::Object(m)
}

struct Rendered {
    file: String,
    content: String,
    summary: String,
    snapshot_id: Value,
}

/// One pack item → its file. Blackboard docs are the cockpit's own; every other kind is a
/// catalog source (`context-catalog.rs`), rendered the same way the live tools render it.
fn render_item(db: &Connection, worktree: &str, item: &Value, generated: &str, snap_id: &Value) -> Rendered {
    let kind = s(item.get("kind"));
    let file = crate::cockpit__helpers_context_pack::item_file(item);
    if kind == "doc" {
        let id = item.get("id").and_then(Value::as_i64).unwrap_or(0);
        let row = crate::db_json::query_rows(db, "SELECT id, body_md FROM cockpit_blackboard WHERE id = ? AND kind = 'context'", &[json!(id)]).ok().and_then(|v| v.as_array().and_then(|a| a.first()).cloned());
        let (content, summary) = match row {
            Some(d) => {
                let resolved = crate::cockpit__helpers_context_brief::resolveContextDocs(db, &json!([d]), &json!([]), &json!({ "db": Value::Null }));
                let r = resolved.get(0).cloned().unwrap_or(json!({}));
                let label = s(r.get("label"));
                (format!("<!-- antiloki context · kind=doc · id={id} · generated={generated} -->\n\n{}", s(r.get("body"))), format!("context doc #{id} — {}", take(&label, 80)))
            }
            None => (format!("<!-- antiloki context · kind=doc · id={id} · generated={generated} -->\n\n_context doc #{id} no longer exists on the blackboard._"), format!("context doc #{id} — removed")),
        };
        return Rendered { file, content, summary, snapshot_id: Value::Null };
    }
    let mut args: Map<String, Value> = item.get("args").and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(p) = nz(item.get("path")) { args.insert("path".into(), p.clone()); }
    match crate::cockpit__helpers_context_catalog::run(db, worktree, &kind, &Value::Object(args), generated) {
        Ok(out) => Rendered { file, content: out.content, summary: out.summary, snapshot_id: if matches!(kind.as_str(), "file" | "folder") { snap_id.clone() } else { Value::Null } },
        Err(e) => Rendered { file, content: format!("<!-- antiloki context · kind={kind} · generated={generated} -->\n\n_{kind} could not be produced: {e}_"), summary: format!("{kind}: not produced — {}", take(&e, 100)), snapshot_id: Value::Null },
    }
}

/// The MIN rule in the agent's words — INDEX.md carries it in full, a refusal carries its first sentence.
pub fn min_rule_md(scope: &[String]) -> String {
    let scope_s = if scope.is_empty() { "your edit scope".to_string() } else { scope.join(", ") };
    format!("This pane is MIN: you navigate with antiloki's measured intel, never by exploring the tree. Grep, Glob, find, ls, tree and reading source files outside your edit scope are REFUSED by the hook — do not retry them or work around them through the shell. Your eyes are this pack (the file reviews, the folder papers, the import graph, the test map) and the live tools: antiloki_file_review, antiloki_folder_review, antiloki_blast_radius, antiloki_tests_for, antiloki_context_index. Read a source file only when you are about to edit it, inside your scope ({scope_s}). When neither the pack nor the tools know a file the ask needs, say so in your answer — `no intel for <path>` — instead of looking for it; the operator will switch the pane out of MIN if the work needs the tree.")
}
fn index_md(pack: &Value, manifest: &Value, snap: &Value, head_sha: &str, coverage_md: &str) -> String {
    let items = manifest.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out: Vec<String> = vec![
        format!("<!-- antiloki context · kind=index · generated={} · hash={} -->", s(manifest.get("generated")), s(manifest.get("hash"))),
        String::new(),
        "# Your context pack".into(),
        String::new(),
        format!(
            "Read this first. These files are antiloki's MEASURED view of the code you were assigned — generated at {} from judge snapshot #{} ({}, engineering_version {}), worktree HEAD {}. Prefer them to re-reading the whole tree: a file review tells you the weak axes, the violated laws with exact line numbers, and who imports the file; `laws.md` is the ruleset your edits are judged against. When a pack file and the source disagree, the source is newer — say so. Before you report done, re-check every file you edited (`antiloki_check` tool, or re-read its review after the next judge run).",
            s(manifest.get("generated")), int(snap.get("id")), take(&s(snap.get("created_at")), 16), s(snap.get("engineering_version")), if head_sha.is_empty() { "unknown".to_string() } else { take(head_sha, 12) },
        ),
        String::new(),
        format!("preset: **{}** · rule: {}", s(pack.get("preset")), if s(pack.get("preset")) == "scope" { s(pack.get("default_rule")) } else { "as selected by the operator".into() }),
        String::new(),
    ];
    if s(manifest.get("mode")) == "min" {
        out.push("## MIN mode — exploration is off".into());
        out.push(String::new());
        out.push(min_rule_md(&manifest.get("scope").and_then(Value::as_array).map(|a| a.iter().map(|x| s(Some(x))).collect::<Vec<_>>()).unwrap_or_default()));
        out.push(String::new());
    }
    // WHERE THE INTEL STOPS (2026-10-01): the files this pack's ask touches that the index does not cover, and the folders it never looks in -
    // said BEFORE the file list, so "imported by 0 files" is read against it. An intel-only agent used to treat the pack as complete.
    if !coverage_md.is_empty() {
        out.push(coverage_md.to_string());
        out.push(String::new());
    }
    out.push("## Files".into());
    for it in items.iter().filter(|i| i.get("written").and_then(Value::as_bool).unwrap_or(false)) {
        out.push(format!("- `{}` — {} ({} chars)", s(it.get("file")), s(it.get("summary")), int(it.get("chars"))));
    }
    let skipped: Vec<&Value> = items.iter().filter(|i| !i.get("written").and_then(Value::as_bool).unwrap_or(false)).collect();
    if !skipped.is_empty() {
        out.push(String::new());
        out.push(format!("## Not materialised (budget {} chars reached) — ask the live tools", int(pack.get("budget_chars"))));
        for it in skipped { out.push(format!("- {} `{}`", s(it.get("kind")), s(it.get("path")))); }
    }
    if pack.get("live").and_then(Value::as_bool).unwrap_or(false) {
        out.push(String::new());
        out.push("## Live tools (MCP server `antiloki`)".into());
        for (n, d) in crate::cockpit__helpers_pane_brief_md::MCP_TOOLS { out.push(format!("- `{n}` — {d}")); }
    }
    out.join("\n")
}

fn write_all(dir: &Path, files: &[(String, String)]) -> Result<(), String> {
    for (rel, content) in files {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
        std::fs::write(&p, content).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

/// `opencode.json` learns the pack and the MCP server — opencode's own `instructions` array is the
/// native way to inject a file into its prompt; `mcp.<name>` is its server table.
/// The pane's SHELL policy for OpenCode (operator 2026-09-20: the TUI asked "Allow once / always" for
/// every command). `settings.shell`: "ask" (OpenCode's default, per command) or "allow" (the box is
/// the agent's own worktree — commands run without asking, like Claude's auto mode). Pure — tested.
/// MIN (operator 2026-09-21: "the opposite of MAX — reads just antiloki's intel, never explores the
/// files"): `settings.min` → "min"; `settings.max` → "max"; else "". The hooks and the plugin read it
/// from the manifest; INDEX.md and the boot prompt say it in words. Pure — tested.
pub fn pane_mode(settings_json: Option<&str>) -> &'static str {
    let v = settings_json.and_then(|t| serde_json::from_str::<Value>(t).ok());
    let on = |k: &str| v.as_ref().and_then(|v| v.get(k)).and_then(Value::as_bool).unwrap_or(false);
    if on("min") { "min" } else if on("max") { "max" } else { "" }
}
pub fn opencode_bash_policy(settings_json: Option<&str>) -> &'static str {
    let v = settings_json.and_then(|t| serde_json::from_str::<Value>(t).ok());
    match v.as_ref().and_then(|v| v.get("shell")).and_then(Value::as_str) { Some("allow") => "allow", _ => "ask" }
}
/// OpenCode's `permission` as a table. One word (`"ask"`) means that word for everything, so it is kept as the general rule and nothing is loosened by adding to it. Pure.
fn permission_map(obj: &Map<String, Value>) -> Map<String, Value> { match obj.get("permission") { Some(Value::Object(m)) => m.clone(), Some(Value::String(w)) => Map::from_iter([("*".to_string(), json!(w))]), _ => Map::new() } }
/// OpenCode: antiloki's own tools are not asked about (operator 2026-11-28), whatever the general rule says; what the person wrote for this name is kept. Pure.
fn allow_antiloki_tools(perm: &mut Map<String, Value>, live: bool) { if live { perm.entry("antiloki_*").or_insert(json!("allow")); } else if perm.get("antiloki_*") == Some(&json!("allow")) { perm.remove("antiloki_*"); } }
/// `create`: the pane IS an OpenCode pane, so a missing `opencode.json` is made (2026-11-25, seen live: a plain OpenCode seat has no
/// proxy file, so antiloki's tools were never wired and the connection gate could never lift). Other engines only ever patch one that exists.
fn patch_opencode_json(wt: &Path, pane_id: i64, live: bool, bash: &str, antiqa: Option<&Value>, create: bool) {
    if hands_off(wt, "opencode.json") { return; }
    let p = wt.join("opencode.json");
    let text = match std::fs::read_to_string(&p) { Ok(t) => t, Err(_) if create && live => "{}".to_string(), Err(_) => return };
    let Ok(mut cfg) = serde_json::from_str::<Value>(&text) else { return };
    let Some(obj) = cfg.as_object_mut() else { return };
    obj.insert("instructions".into(), json!([format!("{CONTEXT_DIR}/INDEX.md")]));
    // the paid model older builds pinned here is taken back out, so a box made before this change follows the person's own default too
    if matches!(obj.get("model").and_then(Value::as_str), Some("opencode/deepseek-v4-pro" | "opencode/deepseek-v4-flash")) { obj.remove("model"); }
    let mut perm = permission_map(obj);
    perm.insert("bash".into(), json!(bash));
    allow_antiloki_tools(&mut perm, live);
    obj.insert("permission".into(), Value::Object(perm));
    let mut mcp = obj.get("mcp").and_then(Value::as_object).cloned().unwrap_or_default();
    if live {
        let entry = mcp_server_entry(pane_id, &wt.to_string_lossy());
        let mut cmd: Vec<Value> = vec![entry["command"].clone()];
        cmd.extend(entry["args"].as_array().cloned().unwrap_or_default());
        mcp.insert("antiloki".into(), json!({ "type": "local", "command": cmd, "enabled": true, "environment": entry["env"].clone() }));
    } else {
        mcp.remove("antiloki");
    }
    match antiqa.filter(|_| live) {
        Some(a) => {
            let mut cmd: Vec<Value> = vec![a["command"].clone()];
            cmd.extend(a["args"].as_array().cloned().unwrap_or_default());
            mcp.insert("antiqa".into(), json!({ "type": "local", "command": cmd, "enabled": true, "environment": a["env"].clone() }));
        }
        None => { mcp.remove("antiqa"); }
    }
    obj.insert("mcp".into(), Value::Object(mcp));
    let _ = std::fs::write(&p, serde_json::to_string_pretty(&cfg).unwrap_or_default());
    // 2026-10-13: an untracked opencode.json we just rewrote must not show up in git status.
    if wt.join(".git").is_dir() { git_exclude_line(wt, "opencode.json"); }
}

/// `.antiloki/` must never reach a PR: the worktree's own git exclude file (not `.gitignore`, which
/// would itself be a diff).
/// Append one line to this worktree's `info/exclude` when it is not already there. 2026-10-13: new untracked generated files use this so `git status` stays clean.
pub fn git_exclude_line(wt: &Path, line: &str) {
    let Some(rel) = git_out(&["rev-parse", "--git-path", "info/exclude"], wt) else { return };
    let path = if Path::new(&rel).is_absolute() { PathBuf::from(rel) } else { wt.join(rel) };
    let mut cur = std::fs::read_to_string(&path).unwrap_or_default();
    if cur.lines().any(|l| l.trim() == line) { return; }
    if !cur.is_empty() && !cur.ends_with('\n') { cur.push('\n'); }
    cur.push_str(line); cur.push('\n');
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    let _ = std::fs::write(&path, cur);
}

fn git_exclude_antiloki(wt: &Path) {
    let ours: Vec<&str> = [".antiloki/", ".claude/settings.local.json", ".codex/hooks.json", ".opencode/plugins/antiloki-context.js", ".cursor/mcp.json", ".grok/config.toml"].into_iter().chain(crate::cockpit__helpers_seat_files::EXCLUDED_RULES_FILES.iter().copied()).collect();
    for line in ours { git_exclude_line(wt, line); }
}

/// Everything an engine reads besides the pack itself: the MCP config, opencode.json, the
/// Claude/Codex hook + status line, the OpenCode plugin, the git excludes. Idempotent.
fn engine_files(pane: &Value, wt: &Path, wt_str: &str, pane_id: i64, live: bool) {
    let antiqa = if live { antiqa_server_entry(pane, pane_id, wt_str) } else { None };
    if !hands_off(wt, MCP_CONFIG) {
        if live {
            let _ = std::fs::create_dir_all(wt.join(".antiloki"));
            let _ = std::fs::write(wt.join(MCP_CONFIG), serde_json::to_string_pretty(&json!({ "mcpServers": mcp_servers(pane_id, wt_str, antiqa.as_ref()) })).unwrap_or_default());
            if wt.join(".git").is_dir() { git_exclude_line(wt, MCP_CONFIG); }
        } else {
            let _ = std::fs::remove_file(wt.join(MCP_CONFIG));
        }
    }
    patch_opencode_json(wt, pane_id, live, opencode_bash_policy(pane.get("settings_json").and_then(Value::as_str)), antiqa.as_ref(), s(pane.get("pane_kind")).starts_with("opencode"));
    match s(pane.get("pane_kind")).as_str() {
        "claude-code" => write_claude_hook(wt),
        "codex" => write_codex_hook(wt),
        "opencode" | "opencode-direct" => write_opencode_plugin(wt),
        // CURSOR AND GROK READ A PROJECT MCP FILE (probed 2026-10-05: `.cursor/mcp.json`, `.grok/config.toml`), so a chat pane on either seat can reach the antiloki tools - `antiloki_delegate` above all, which is how a
        // front-mode chat hands work to Heimdall. Antigravity (`agy`) keeps its MCP servers in ONE user-global file with no project scope, so a per-pane server cannot be wired there without rewriting the operator's own config.
        // Neither seat has a pre-write hook wired: scope is still enforced at integration, as it is for Gemini.
        "cursor" => write_cursor_mcp(wt, pane_id, live, antiqa.as_ref()),
        "grok" => write_grok_mcp(wt, pane_id, live, antiqa.as_ref()),
        _ => {}
    }
    git_exclude_antiloki(wt);
}

/// The tools' settings alone, in a folder the agent shares with the person: the file its CLI reads them from, merged into what is there. No brief, no hooks, and no permission beyond antiloki's own tools. A file the project tracks is left alone.
pub fn wire_shared_folder(dir: &Path, pane_kind: &str, pane_id: i64, live: bool) {
    let dir_str = dir.to_string_lossy().into_owned();
    match pane_kind {
        "claude-code" => {
            let file = dir.join(MCP_CONFIG);
            if hands_off(dir, MCP_CONFIG) { return; }
            if live { let _ = std::fs::create_dir_all(dir.join(".antiloki")); let _ = std::fs::write(file, serde_json::to_string_pretty(&json!({ "mcpServers": mcp_servers(pane_id, &dir_str, None) })).unwrap_or_default()); }
            else { let _ = std::fs::remove_file(file); }
        }
        "cursor" => write_cursor_mcp(dir, pane_id, live, None),
        "grok" => write_grok_mcp(dir, pane_id, live, None),
        "opencode" | "opencode-direct" => {
            if hands_off(dir, "opencode.json") { return; }
            let p = dir.join("opencode.json");
            let text = match std::fs::read_to_string(&p) { Ok(t) => t, Err(_) if live => "{}".to_string(), Err(_) => return };
            let Ok(mut cfg) = serde_json::from_str::<Value>(&text) else { return };
            let Some(obj) = cfg.as_object_mut() else { return };
            let mut mcp = obj.get("mcp").and_then(Value::as_object).cloned().unwrap_or_default();
            if live { let e = mcp_server_entry(pane_id, &dir_str); let mut cmd: Vec<Value> = vec![e["command"].clone()]; cmd.extend(e["args"].as_array().cloned().unwrap_or_default()); mcp.insert("antiloki".into(), json!({ "type": "local", "command": cmd, "enabled": true, "environment": e["env"].clone() })); }
            else { mcp.remove("antiloki"); }
            obj.insert("mcp".into(), Value::Object(mcp));
            if live || obj.get("permission").is_some_and(Value::is_object) {
                let mut perm = permission_map(obj);
                allow_antiloki_tools(&mut perm, live);
                if !perm.is_empty() { obj.insert("permission".into(), Value::Object(perm)); } else { obj.remove("permission"); }
            }
            let _ = std::fs::write(&p, serde_json::to_string_pretty(&cfg).unwrap_or_default());
            if dir.join(".git").is_dir() { git_exclude_line(dir, "opencode.json"); }
        }
        _ => {}   // Codex takes them on its command line; Antigravity from its own file
    }
}

/// What the pack is FOR, as text the coverage check can read: the operator's guidance on the pane and the session's mission.
fn coverage_ask(db: &Connection, pane: &Value) -> String {
    let mission = crate::db_json::query_rows(db, "SELECT mission_md FROM cockpit_sessions WHERE id = ?", &[pane.get("session_id").cloned().unwrap_or(Value::Null)]).ok()
        .and_then(|v| v.as_array().and_then(|a| a.first()).and_then(|r| r.get("mission_md")).and_then(Value::as_str).map(String::from)).unwrap_or_default();
    format!("{}\n{}", s(pane.get("extra_prompt_md")), mission)
}

fn rewrite_brief(db: &Connection, pane: &Value, wt: &Path, manifest: Option<&Value>, live: bool) {
    let kind = s(pane.get("pane_kind"));
    if !CODE_AGENT_KINDS.contains(&kind.as_str()) { return; }
    let scope = crate::cockpit__helpers_context_pack::pane_scope(pane);
    let session_id = pane.get("session_id").cloned().unwrap_or(Value::Null);
    let docs = crate::db_json::query_rows(db, "SELECT id, body_md FROM cockpit_blackboard WHERE session_id = ? AND kind = 'context'", &[session_id]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let extra = nz(pane.get("extra_prompt_md")).map(|v| s(Some(v)));
    // THE INTRODUCTION TO ANTILOKI comes from the prompt catalog's active row (SQLite, editable in Settings), so every engine reads the same text
    let intro: Option<String> = db.query_row("SELECT template_md FROM brain.prompt_templates WHERE name = 'agent.antiloki_intro' AND superseded_by IS NULL ORDER BY version DESC LIMIT 1", [], |r| r.get(0)).ok();
    let brief = crate::cockpit__helpers_pane_brief_md::brief_md(&crate::cockpit__helpers_pane_brief_md::BriefInput {
        intro: intro.as_deref(),
        extra_prompt_md: extra.as_deref(),
        scope: &scope,
        manifest,
        docs: &docs,
        live_tools: live,
        antiqa: live && antiqa_server_entry(pane, pane.get("id").and_then(Value::as_i64).unwrap_or(0), &wt.to_string_lossy()).is_some(),
    });
    if !hands_off(wt, "AGENTS.md") {
        crate::cockpit__helpers_pane_brief_md::upsert_brief(&wt.join("AGENTS.md"), &brief);
        if wt.join(".git").is_dir() { git_exclude_line(wt, "AGENTS.md"); }
    }
    if kind == "claude-code" && !hands_off(wt, "CLAUDE.md") {
        crate::cockpit__helpers_pane_brief_md::upsert_brief(&wt.join("CLAUDE.md"), &brief);
        if wt.join(".git").is_dir() { git_exclude_line(wt, "CLAUDE.md"); }
    }
    seat_native(db, pane, wt);
}

/// THE AGENT'S CRAFT IN ITS SEAT'S OWN PLACE (2026-10-01, "skills and specialists are not materialized for Cursor agents"). An agent hired
/// with skills gets them written where its CLI loads them (`seat-files.rs`: `.cursor/skills`, `.grok/skills`, `.agents/skills`, ...), and an
/// agent hired with skills or specialists gets its seat's rules file listing them with the paths THAT seat reads. Runs with every brief
/// rewrite, so it follows a seat switch and a recipe edit. A pane hired with neither is left alone (the skill library's own button still works).
fn seat_native(db: &Connection, pane: &Value, wt: &Path) {
    let settings: Value = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    // `skills: []` is a decision (the agent was hired with none) and clears what an earlier start wrote; an absent key is "nobody said", and leaves the skill library's own button alone
    let mut skills: Vec<crate::cockpit__helpers_seat_files::SkillRef> = Vec::new();
    if settings.get("skills").map_or(false, Value::is_array) {
        if let Ok(r) = crate::agents_in_skill_library_materialize::skillLibraryMaterialize(&json!({ "pane_id": pane.get("id").cloned().unwrap_or(Value::Null) }), db) {
            // the built-in context skill rides with every pack; it is not something the agent was "hired with"
            skills = r.get("skills").and_then(Value::as_array).map(|a| a.iter().filter(|x| s(x.get("slug")) != "antiloki-context").map(|x| crate::cockpit__helpers_seat_files::SkillRef { slug: s(x.get("slug")), name: s(x.get("name")), description: s(x.get("description")) }).collect()).unwrap_or_default();
        }
    }
    let _ = crate::cockpit__helpers_seat_files::write_rules(db, pane, wt, &skills);
}

fn save_settings(db: &Connection, pane_id: i64, settings: &Value, hash: Option<&str>, baked_at: Option<&str>) {
    let mut m: Map<String, Value> = match settings {
        Value::String(raw) => serde_json::from_str::<Value>(raw).ok().and_then(|v| v.as_object().cloned()).unwrap_or_default(),
        Value::Object(o) => o.clone(),
        _ => Map::new(),
    };
    match hash { Some(h) => { m.insert("context_pack_hash".into(), json!(h)); } None => { m.remove("context_pack_hash"); } }
    match baked_at { Some(t) => { m.insert("context_pack_baked_at".into(), json!(t)); } None => { m.remove("context_pack_baked_at"); } }
    let _ = crate::db_json::run_write(db, "UPDATE cockpit_panes SET settings_json = ? WHERE id = ?", &[json!(serde_json::to_string(&Value::Object(m)).unwrap_or_default()), json!(pane_id)]);
}

#[allow(non_snake_case)]
pub fn cockpitPaneContextBake(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let pane_id = input.get("pane_id").and_then(Value::as_i64).or_else(|| input.get("pane_id").and_then(Value::as_str).and_then(|x| x.parse().ok())).unwrap_or(0);
    if pane_id <= 0 { return Err("cockpit-pane-context-bake: pane_id is required".into()); }
    let force = input.get("force").and_then(Value::as_bool).unwrap_or(false);

    let pane = match crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[json!(pane_id)])?.as_array().and_then(|a| a.first()).cloned() {
        Some(p) => p,
        None => return Ok(json!({ "ok": false, "error": format!("pane {pane_id} not found") })),
    };
    let wt_str = s(pane.get("worktree_path"));
    let wt = PathBuf::from(&wt_str);
    // AN AGENT WITH NO WORKTREE STILL GETS ANTILOKI'S TOOLS (2026-11-27, a Windows PC: a project folder that is not a Git repository made no worktree, so nothing was set up and every agent ran without them, saying nothing).
    // Only when the caller is about to start it (`shared`): the tools' settings go into the project folder it will run in, and nothing else.
    if wt_str.is_empty() && input.get("shared").and_then(Value::as_bool) == Some(true) {
        // the folder the caller is about to start it in (a thread's worktree), else the project folder
        let root = input.get("folder").and_then(Value::as_str).filter(|f| Path::new(f).is_dir()).map(String::from).unwrap_or_else(|| crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default().to_string());
        if !root.is_empty() && Path::new(&root).is_dir() {
            let live = crate::cockpit__helpers_context_pack::resolve_pack(db, &pane).get("live").and_then(Value::as_bool).unwrap_or(true);
            wire_shared_folder(Path::new(&root), &s(pane.get("pane_kind")), pane_id, live);
            return Ok(json!({ "ok": true, "shared": true, "live": live, "items": [], "mcp_server": if live { mcp_server_entry(pane_id, &root) } else { Value::Null }, "antiqa_server": Value::Null }));
        }
    }
    if wt_str.is_empty() || !wt.is_dir() {
        return Ok(json!({ "ok": false, "pending": true, "error": "the pane's worktree is pending — nothing to bake into yet" }));
    }

    let pack = crate::cockpit__helpers_context_pack::resolve_pack(db, &pane);
    let items = pack.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let live = pack.get("live").and_then(Value::as_bool).unwrap_or(true);
    let antiqa_srv = if live { antiqa_server_entry(&pane, pane_id, &wt_str) } else { None };
    let budget = pack.get("budget_chars").and_then(Value::as_i64).unwrap_or(crate::cockpit__helpers_context_pack::DEFAULT_BUDGET_CHARS);
    let ctx_dir = wt.join(CONTEXT_DIR);

    // preset none → take the pack away, keep the tools if wanted, brief goes back to inlining docs
    if items.is_empty() {
        let _ = std::fs::remove_dir_all(&ctx_dir);
        // 2026-10-13: a tracked mcp.json in a main checkout is neither rewritten nor deleted.
        if !hands_off(&wt, MCP_CONFIG) {
            if live {
                let _ = std::fs::create_dir_all(wt.join(".antiloki"));
                let _ = std::fs::write(wt.join(MCP_CONFIG), serde_json::to_string_pretty(&json!({ "mcpServers": mcp_servers(pane_id, &wt_str, antiqa_srv.as_ref()) })).unwrap_or_default());
                if wt.join(".git").is_dir() { git_exclude_line(&wt, MCP_CONFIG); }
            } else {
                let _ = std::fs::remove_file(wt.join(MCP_CONFIG));
            }
        }
        patch_opencode_json(&wt, pane_id, live, opencode_bash_policy(pane.get("settings_json").and_then(Value::as_str)), antiqa_srv.as_ref(), s(pane.get("pane_kind")).starts_with("opencode"));
        save_settings(db, pane_id, pane.get("settings_json").unwrap_or(&Value::Null), None, None);
        rewrite_brief(db, &pane, &wt, None, live);
        return Ok(json!({ "ok": true, "removed": true, "path": CONTEXT_DIR, "files": [], "chars": 0, "live": live, "warnings": pack.get("warnings").cloned().unwrap_or(json!([])), "mcp_server": if live { mcp_server_entry(pane_id, &wt_str) } else { Value::Null }, "antiqa_server": antiqa_srv.clone().unwrap_or(Value::Null) }));
    }

    let snap = crate::db_json::query_rows(db, "SELECT id, created_at, engineering_version, constitution_hash FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1", &[])
        .ok().and_then(|v| v.as_array().and_then(|a| a.first()).cloned()).unwrap_or(json!({}));
    let head_sha = git_out(&["rev-parse", "HEAD"], &wt).unwrap_or_default();
    // THE COVERAGE CHECK reads the ask (the operator's guidance + the session's mission) and the files the pack selects, and compares them with the
    // index (`coverage-guard.rs`). It is part of the pack's identity: an index rebuilt with a different scope must not be answered by the old notice.
    let ask = coverage_ask(db, &pane);
    let pack_files: Vec<String> = items.iter().filter_map(|i| i.get("path").and_then(Value::as_str)).filter(|p| !p.is_empty() && wt.join(p).is_file()).map(String::from).collect();
    let coverage = crate::codebase_coverage_guard::check(db, &wt, "atlas", &ask, &pack_files);
    let coverage_md = crate::codebase_coverage_guard::notice_md(&coverage);
    let hash = {
        let base = crate::cockpit__helpers_context_pack::pack_hash(&items, snap.get("id").and_then(Value::as_i64), &s(snap.get("engineering_version")), &s(snap.get("constitution_hash")), &head_sha, budget);
        crate::codebase_atlas_ingest::hashSource(&format!("{base}|{coverage_md}"))
    };
    let prev_hash: String = pane.get("settings_json").and_then(Value::as_str).and_then(|raw| serde_json::from_str::<Value>(raw).ok()).and_then(|v| v.get("context_pack_hash").and_then(Value::as_str).map(String::from)).unwrap_or_default();
    if !force && prev_hash == hash && ctx_dir.join("INDEX.md").is_file() {
        // the pack is current, but the engine wiring (hook · status line · plugin · mcp config) is
        // cheap and idempotent — keep it current too, so a BE upgrade reaches a running pane
        engine_files(&pane, &wt, &wt_str, pane_id, live);
        let manifest: Value = std::fs::read_to_string(ctx_dir.join("manifest.json")).ok().and_then(|m| serde_json::from_str(&m).ok()).unwrap_or(json!({}));
        // …AND SO IS THE BRIEF (operator 2026-09-28: "I FUCKING TOLD YOU ALL AGENTS CAN EDIT NOW").
        //
        // `AGENTS.md`/`CLAUDE.md` carry the PANE BRIEF — scope, mission, operator guidance — and it is the most
        // authoritative thing the model reads. It was rewritten only when the PACK changed, and the brief does not
        // depend on the pack: it depends on the pane's scope and guidance, neither of which is in `pack_hash`. So
        // main's brief sat five days old still saying "reads the whole workspace, changes nothing", and the agent
        // refused to edit a file it had every permission to edit — correctly, because that sentence outranked
        // every flag in the machine.
        //
        // It is two string writes. Cheap and idempotent, exactly like the engine wiring above, and for the same
        // reason: a running pane must not be left reading something that is no longer true.
        rewrite_brief(db, &pane, &wt, Some(&manifest), live);
        return Ok(json!({ "ok": true, "skipped": true, "hash": hash, "path": CONTEXT_DIR, "live": live, "items": manifest.get("items").cloned().unwrap_or(json!([])), "chars": manifest.get("chars").cloned().unwrap_or(json!(0)), "generated": manifest.get("generated").cloned().unwrap_or(Value::Null), "mcp_server": if live { mcp_server_entry(pane_id, &wt_str) } else { Value::Null }, "antiqa_server": antiqa_srv.clone().unwrap_or(Value::Null) }));
    }

    let generated = crate::clock::now_iso(db);
    let mut files: Vec<(String, String)> = Vec::new();
    let mut manifest_items: Vec<Value> = Vec::new();
    let mut total: i64 = 0;
    let snap_id = snap.get("id").cloned().unwrap_or(Value::Null);
    for it in &items {
        let r = render_item(db, &wt_str, it, &generated, &snap_id);
        let chars = r.content.chars().count() as i64;
        let written = total + chars <= budget;
        if written { total += chars; files.push((r.file.clone(), r.content)); }
        let mut m = it.as_object().cloned().unwrap_or_default();
        m.insert("file".into(), json!(r.file));
        m.insert("summary".into(), json!(r.summary));
        m.insert("chars".into(), json!(chars));
        m.insert("written".into(), json!(written));
        if !r.snapshot_id.is_null() { m.insert("snapshot_id".into(), r.snapshot_id); }
        manifest_items.push(Value::Object(m));
    }
    let manifest = json!({
        "hash": hash,
        "generated": generated,
        "engineering_version": snap.get("engineering_version").cloned().unwrap_or(Value::Null),
        "constitution_hash": snap.get("constitution_hash").cloned().unwrap_or(Value::Null),
        "snapshot_id": snap.get("id").cloned().unwrap_or(Value::Null),
        "snapshot_created_at": snap.get("created_at").cloned().unwrap_or(Value::Null),
        "head_sha": head_sha,
        "preset": pack.get("preset").cloned().unwrap_or(Value::Null),
        "budget_chars": budget,
        "chars": total,
        "live": live,
        // the edit whitelist rides in the manifest so the hooks/plugin can refuse an out-of-scope write BEFORE it lands
        "scope": crate::cockpit__helpers_context_pack::pane_scope(&pane),
        // MIN rides in the manifest too: the hooks refuse exploration, the plugin likewise
        "mode": pane_mode(pane.get("settings_json").and_then(Value::as_str)),
        "items": manifest_items,
        "coverage": { "gaps": coverage.gaps.len(), "checked": coverage.checked, "blind_spot_folders": coverage.blind.len(), "terms": coverage.terms, "brief_line": crate::codebase_coverage_guard::brief_line(&coverage) },
    });
    files.push(("INDEX.md".into(), index_md(&pack, &manifest, &snap, &head_sha, &coverage_md)));
    files.push(("manifest.json".into(), serde_json::to_string_pretty(&manifest).unwrap_or_default()));

    // write aside, then swap in
    let tmp = wt.join(format!(".antiloki/context.tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    write_all(&tmp, &files)?;
    let _ = std::fs::remove_dir_all(&ctx_dir);
    std::fs::rename(&tmp, &ctx_dir).map_err(|e| format!("swap in the pack: {e}"))?;

    engine_files(&pane, &wt, &wt_str, pane_id, live);
    save_settings(db, pane_id, pane.get("settings_json").unwrap_or(&Value::Null), Some(&hash), Some(&generated));
    rewrite_brief(db, &pane, &wt, Some(&manifest), live);

    let written_files: Vec<String> = files.iter().map(|(f, _)| f.clone()).collect();
    let skipped_items: Vec<Value> = manifest["items"].as_array().unwrap().iter().filter(|i| !i["written"].as_bool().unwrap_or(false)).cloned().collect();
    Ok(json!({
        "ok": true,
        "skipped": false,
        "hash": hash,
        "path": CONTEXT_DIR,
        "files": written_files,
        "chars": total,
        "live": live,
        "items": manifest["items"],
        "skipped_items": skipped_items,
        "warnings": pack.get("warnings").cloned().unwrap_or(json!([])),
        "generated": manifest["generated"],
        "mcp_server": if live { mcp_server_entry(pane_id, &wt_str) } else { Value::Null }, "antiqa_server": antiqa_srv.clone().unwrap_or(Value::Null),
    }))
}

/// The pack as ONE prompt string for panes that have no file reader (`agent` panes): INDEX.md first,
/// then every materialised file in manifest order, cut at `cap` chars with the cut stated. `None`
/// when the worktree has no pack.
pub const PACK_PROMPT_CAP: usize = 24000;
pub fn pack_prompt_text(worktree: &str, cap: usize) -> Option<String> {
    if worktree.is_empty() { return None; }
    let dir = Path::new(worktree).join(CONTEXT_DIR);
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).ok()?).ok()?;
    let mut out = std::fs::read_to_string(dir.join("INDEX.md")).ok()?;
    let items = manifest.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut cut: Vec<String> = Vec::new();
    for it in items.iter().filter(|i| i.get("written").and_then(Value::as_bool).unwrap_or(false)) {
        let f = s(it.get("file"));
        let Ok(body) = std::fs::read_to_string(dir.join(&f)) else { continue };
        let piece = format!("\n\n===== .antiloki/context/{f} =====\n{body}");
        if out.chars().count() + piece.chars().count() > cap { cut.push(f); continue; }
        out.push_str(&piece);
    }
    if !cut.is_empty() { out.push_str(&format!("\n\n(cut at {cap} chars — not inlined: {})", cut.join(", "))); }
    Some(out)
}

/// `system.cockpit.pane.context.refresh` — bake every provisioned pane (hash-skips no-ops). The
/// atlas watcher calls this after a codebase sync; the FE after a judge run.
#[allow(non_snake_case)]
pub fn cockpitPaneContextRefresh(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let session_filter = input.get("session_id").and_then(Value::as_i64);
    let rows = match session_filter {
        Some(sid) => crate::db_json::query_rows(db, "SELECT id FROM cockpit_panes WHERE worktree_path IS NOT NULL AND session_id = ? ORDER BY id", &[json!(sid)])?,
        None => crate::db_json::query_rows(db, "SELECT id FROM cockpit_panes WHERE worktree_path IS NOT NULL ORDER BY id", &[])?,
    };
    let mut baked = 0i64; let mut skipped = 0i64; let mut pending = 0i64; let mut failed: Vec<Value> = Vec::new();
    for r in rows.as_array().cloned().unwrap_or_default() {
        let id = r.get("id").and_then(Value::as_i64).unwrap_or(0);
        match cockpitPaneContextBake(&json!({ "pane_id": id, "force": input.get("force").cloned().unwrap_or(json!(false)) }), db) {
            Ok(v) if v.get("ok") == Some(&json!(true)) => { if v.get("skipped") == Some(&json!(true)) { skipped += 1 } else { baked += 1 } }
            // a row whose box is gone from disk is not a failure of the refresh — it has nothing to bake into
            Ok(v) if v.get("pending") == Some(&json!(true)) => pending += 1,
            Ok(v) => failed.push(json!({ "pane_id": id, "error": v.get("error").cloned().unwrap_or(Value::Null) })),
            Err(e) => failed.push(json!({ "pane_id": id, "error": e })),
        }
    }
    Ok(json!({ "ok": true, "baked": baked, "skipped": skipped, "pending": pending, "failed": failed }))
}

/// `system.cockpit.pane.context.tool` — the ONE endpoint behind the `antiloki` MCP server. Every
/// tool is a dispatch + a review-markdown renderer, so a live answer and a pack file are the same
/// bytes for the same snapshot. input: `{pane_id, tool, args}`; returns `{ok, markdown}`.
#[allow(non_snake_case)]
pub fn cockpitPaneContextTool(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let tool = s(input.get("tool"));
    let args = input.get("args").cloned().unwrap_or(json!({}));
    let pane_id = input.get("pane_id").and_then(Value::as_i64).or_else(|| input.get("pane_id").and_then(Value::as_str).and_then(|x| x.parse().ok())).unwrap_or(0);
    let pane = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[json!(pane_id)])?.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    let mut wt = s(pane.get("worktree_path"));
    // PANE 0 = THE WORKSPACE ITSELF (a team's X-ray node has no terminal pane): the same tools read the project the serve is bound to
    if wt.is_empty() && pane_id == 0 { wt = crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or("").to_string(); }
    let generated = crate::clock::now_iso(db);

    // the seven named tools are catalog sources with a fixed key; `antiloki_query` opens the whole
    // catalog (any antiIDE feature); `antiloki_sources` lists it.
    let (source, mut sargs): (String, Map<String, Value>) = match tool.as_str() {
        "antiloki_context_index" => {
            let md = if wt.is_empty() { "_no worktree for this pane_".to_string() } else {
                std::fs::read_to_string(Path::new(&wt).join(CONTEXT_DIR).join("INDEX.md")).unwrap_or_else(|_| "_no context pack baked for this pane yet — the operator can bake one from the pane's context chip._".to_string())
            };
            return Ok(json!({ "ok": true, "tool": tool, "markdown": md }));
        }
        "antiloki_reads" => {
            let r = if wt.is_empty() { json!({ "total": 0 }) } else { reads_summary(Path::new(&wt)) };
            let md = format!("# Reads\n\npack {} · source {} · search {} · live tools {} · brief {} · nudges {} · INDEX read: {}", int(r.get("pack")), int(r.get("source")), int(r.get("search")), int(r.get("tool")), int(r.get("brief")), int(r.get("nudged")), if r.get("index_read") == Some(&json!(true)) { "yes" } else { "no" });
            return Ok(json!({ "ok": true, "tool": tool, "markdown": md, "reads": r }));
        }
        "antiloki_plans" => {
            if wt.is_empty() { return Ok(json!({ "ok": true, "tool": tool, "markdown": "_no worktree_", "plans": [] })); }
            let wtp = Path::new(&wt);
            let mut plans = plans_summary(wtp, &[]);
            let want = s(args.get("file"));
            for p in plans.iter_mut() {
                let f = s(p.get("file"));
                if want.is_empty() || want == f {
                    let body = std::fs::read_to_string(wtp.join(&f)).unwrap_or_default();
                    p["body_md"] = json!(body);
                }
            }
            let md = plans.iter().map(|p| format!("## {} — {}\n\n{}", s(p.get("title")), s(p.get("status")), s(p.get("body_md")))).collect::<Vec<_>>().join("\n\n---\n\n");
            return Ok(json!({ "ok": true, "tool": tool, "markdown": if md.is_empty() { "_no plans yet_".to_string() } else { md }, "plans": plans }));
        }
        "antiloki_plan_decide" => {
            if wt.is_empty() { return Err("no worktree".into()); }
            let file = s(args.get("file"));
            let decision = s(args.get("decision"));
            if !file.starts_with(".antiloki/plans/") || !matches!(decision.as_str(), "accepted" | "rejected") { return Err("file (.antiloki/plans/…) and decision (accepted|rejected) are required".into()); }
            let dir = Path::new(&wt).join(".antiloki").join("plans");
            let _ = std::fs::create_dir_all(&dir);
            let line = json!({ "t": crate::clock::now_iso(db), "file": file, "decision": decision, "reason": args.get("reason").cloned().unwrap_or(Value::Null) });
            let mut cur = std::fs::read_to_string(dir.join("decisions.jsonl")).unwrap_or_default();
            cur.push_str(&line.to_string()); cur.push('\n');
            std::fs::write(dir.join("decisions.jsonl"), cur).map_err(|e| e.to_string())?;
            return Ok(json!({ "ok": true, "tool": tool, "markdown": format!("plan {file}: {decision}"), "decision": line }));
        }
        "antiloki_audit" => {
            if wt.is_empty() { return Ok(json!({ "ok": true, "tool": tool, "markdown": "_no worktree_", "audit": Value::Null })); }
            let base = pane.get("session_id").and_then(Value::as_i64)
                .and_then(|sid| crate::db_json::query_rows(db, "SELECT base_branch FROM cockpit_sessions WHERE id = ?", &[json!(sid)]).ok())
                .and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|x| x.get("base_branch")).and_then(Value::as_str).map(String::from))
                .unwrap_or_default();
            let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(500).max(1) as usize;
            let a = audit_summary(Path::new(&wt), &base, &s(args.get("op")), &s(args.get("path")), limit);
            return Ok(json!({ "ok": true, "tool": tool, "markdown": audit_md(&a), "audit": a }));
        }
        "antiloki_answers" => {
            let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(20).max(1) as usize;
            let kind = s(pane.get("pane_kind"));
            let answers: Vec<Value> = if wt.is_empty() { Vec::new() } else if kind == "opencode" || kind == "opencode-direct" {
                crate::cockpit__helpers_engine_usage::opencode_answers(Path::new(&wt), limit)
            } else {
                std::fs::read_to_string(Path::new(&wt).join(".antiloki").join("answers.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect::<Vec<_>>().into_iter().rev().take(limit).collect()
            };
            let md = answers.iter().map(|a| format!("### {}\n\n{}", s(a.get("t")), s(a.get("text")))).collect::<Vec<_>>().join("\n\n---\n\n");
            return Ok(json!({ "ok": true, "tool": tool, "markdown": if md.is_empty() { "_no answers recorded yet_".to_string() } else { md }, "answers": answers }));
        }
        // THE SPECIALISTS (operator 2026-09-19: "all specialists and skills — so we can choose what is
        // available to the agent"): the registry filtered by the pane's whitelist (settings.specialists;
        // absent = every ready specialist), and one isolated review call on the subscription.
        "antiloki_specialists" => {
            let avail = pane_specialists(db, &pane);
            let md = if avail.is_empty() { "_no specialist is available to this agent — the operator picks them in the pane's ⚙ › subagents_".to_string() } else {
                format!("# Specialists available to you\n\ncall `antiloki_specialist {{specialist, path, question?}}` — one isolated review of a file or folder by that lens, on your subscription.\n\n{}",
                    avail.iter().map(|a| format!("- `{}` — {}: {}", s(a.get("id")), s(a.get("title")), s(a.get("description")).chars().take(160).collect::<String>())).collect::<Vec<_>>().join("\n"))
            };
            return Ok(json!({ "ok": true, "tool": tool, "markdown": md, "specialists": avail }));
        }
        "antiloki_specialist" => {
            let want = s(args.get("specialist"));
            let path = s(args.get("path"));
            if want.is_empty() || path.is_empty() { return Ok(json!({ "ok": false, "error": "antiloki_specialist needs {specialist, path} — antiloki_specialists lists the ids" })); }
            let avail = pane_specialists(db, &pane);
            let Some(a) = avail.iter().find(|a| s(a.get("id")) == want || s(a.get("title")).eq_ignore_ascii_case(&want)) else {
                return Ok(json!({ "ok": false, "error": format!("`{want}` is not available to this agent — antiloki_specialists lists what is; the operator picks them in ⚙ › subagents") }));
            };
            let mut inp = json!({ "agent_id": s(a.get("id")), "path": path, "research": false, "sources": [], "tier": "mid", "swarm_id": format!("pane:{pane_id}") });
            let q = s(args.get("question"));
            if !q.is_empty() { inp["custom_instruction"] = json!(q); }
            let r = crate::constitution_constitution_agent_run::constitutionAgentRun(&inp, db)?;
            if r.get("ok") != Some(&json!(true)) { return Ok(json!({ "ok": false, "error": s(r.get("error")) })); }
            let md = specialist_review_md(&r);
            return Ok(json!({ "ok": true, "tool": tool, "markdown": md, "review": r.get("review").cloned().unwrap_or(Value::Null), "ai_call_id": r.get("ai_call_id").cloned().unwrap_or(Value::Null) }));
        }
        "antiloki_usage" => {
            let now = crate::clock::now_iso(db);
            let kind = s(pane.get("pane_kind"));
            let u = if wt.is_empty() { json!({ "engine": kind, "source": "none", "reason": "no worktree", "limits": [] }) } else {
                match kind.as_str() {
                    "claude-code" => crate::cockpit__helpers_engine_usage::claude_usage(Path::new(&wt), pane.get("id").and_then(Value::as_i64).unwrap_or(0), &now),
                    "codex" => crate::cockpit__helpers_engine_usage::codex_usage(Path::new(&wt), &now),
                    "opencode" | "opencode-direct" => crate::cockpit__helpers_engine_usage::opencode_usage(Path::new(&wt), Some(json!({ "cost_usd": pane.get("cost_usd").cloned().unwrap_or(json!(0)), "calls": pane.get("calls").cloned().unwrap_or(json!(0)) })), &now),
                    other => json!({ "engine": other, "source": "none", "reason": "not a CLI engine pane", "limits": [] }),
                }
            };
            return Ok(json!({ "ok": true, "tool": tool, "markdown": crate::cockpit__helpers_engine_usage::usage_md(&u), "usage": u }));
        }
        // THE QUOTA HANDOFF (2026-09-20): export this pane's conversation + the files it touched as
        // `.antiloki/handoff/<stamp>-<engine>.md` so another engine (or this one after the reset) continues
        "antiloki_handoff" => {
            if wt.is_empty() { return Ok(json!({ "ok": false, "error": "no worktree — nothing to hand off" })); }
            let now = crate::clock::now_iso(db);
            let kind = s(pane.get("pane_kind"));
            let to = s(args.get("to")); let reason = s(args.get("reason"));
            let usage = match kind.as_str() {
                "claude-code" => crate::cockpit__helpers_engine_usage::claude_usage(Path::new(&wt), pane.get("id").and_then(Value::as_i64).unwrap_or(0), &now),
                "codex" => crate::cockpit__helpers_engine_usage::codex_usage(Path::new(&wt), &now),
                _ => crate::cockpit__helpers_engine_usage::opencode_usage(Path::new(&wt), None, &now),
            };
            let (source, turns) = crate::cockpit__helpers_engine_handoff::transcript(&kind, Path::new(&wt));
            let base = pane.get("session_id").and_then(Value::as_i64)
                .and_then(|sid| crate::db_json::query_rows(db, "SELECT base_branch FROM cockpit_sessions WHERE id = ?", &[json!(sid)]).ok())
                .and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|x| x.get("base_branch")).and_then(Value::as_str).map(String::from)).unwrap_or_default();
            let audit = audit_summary(Path::new(&wt), &base, "", "", 1);
            let mut files: Vec<(String, String)> = audit.get("files").and_then(Value::as_array).into_iter().flatten()
                .filter(|f| f.get("writes").and_then(Value::as_i64).unwrap_or(0) > 0)
                .map(|f| (s(f.get("path")), format!("written · {} edit{}", f.get("writes").and_then(Value::as_i64).unwrap_or(0), if f.get("writes").and_then(Value::as_i64).unwrap_or(0) == 1 { "" } else { "s" }))).collect();
            for g in audit.get("git").and_then(|g| g.get("changed")).and_then(Value::as_array).into_iter().flatten() { let p = s(Some(g)); if !files.iter().any(|(x, _)| *x == p) { files.push((p, "changed in git".into())); } }
            let scope: Vec<String> = crate::cockpit__helpers_context_pack::pane_scope(&pane);
            let md = crate::cockpit__helpers_engine_handoff::handoff_md(&kind, &to, &reason, &wt, &s(pane.get("branch")), &scope, &usage, &files, &turns, &now);
            let (rel, latest) = crate::cockpit__helpers_engine_handoff::write_handoff(Path::new(&wt), &kind, &md, &now)?;
            let summary = format!("handoff written: `{rel}` (and `{latest}`) — {} turn{} from {}, {} file{} touched. Another engine reads it first and continues.", turns.len(), if turns.len() == 1 { "" } else { "s" }, if source.is_empty() { "no transcript found".to_string() } else { kind.clone() }, files.len(), if files.len() == 1 { "" } else { "s" });
            return Ok(json!({ "ok": true, "tool": tool, "markdown": summary, "file": rel, "latest": latest, "turns": turns.len(), "files": files.len(), "chars": md.chars().count(), "source": source }));
        }
        "antiloki_sources" => {
            return Ok(json!({ "ok": true, "tool": tool, "markdown": crate::cockpit__helpers_context_catalog::catalog_md(), "sources": crate::cockpit__helpers_context_catalog::catalog_json() }));
        }
        "antiloki_query" => {
            let src = s(args.get("source"));
            let inner = args.get("args").and_then(Value::as_object).cloned().unwrap_or_default();
            // args may also ride flat beside `source` (a lenient agent)
            let mut merged = inner;
            if let Some(o) = args.as_object() { for (k, v) in o { if k != "source" && k != "args" { merged.entry(k.clone()).or_insert(v.clone()); } } }
            (src, merged)
        }
        "antiloki_file_review" => ("file".into(), args.as_object().cloned().unwrap_or_default()),
        "antiloki_folder_review" => ("folder".into(), args.as_object().cloned().unwrap_or_default()),
        "antiloki_check" => ("check".into(), args.as_object().cloned().unwrap_or_default()),
        "antiloki_blast_radius" => ("blast".into(), args.as_object().cloned().unwrap_or_default()),
        "antiloki_tests_for" => ("tests".into(), args.as_object().cloned().unwrap_or_default()),
        "antiloki_laws" => ("laws".into(), Map::new()),
        other => return Err(format!("unknown tool `{other}`")),
    };
    // `antiloki_file_review` on a folder path answers with the folder paper (and vice versa)
    if (source == "file" || source == "folder") && !sargs.contains_key("path") { sargs.insert("path".to_string(), Value::Null); }
    let out = crate::cockpit__helpers_context_catalog::run(db, &wt, &source, &Value::Object(sargs), &generated)?;
    let md = out.content;
    Ok(json!({ "ok": true, "tool": tool, "source": source, "summary": out.summary, "markdown": md }))
}

/// The specialists a pane may call: the registry's READY agents, narrowed by `settings.specialists`
/// (a list of ids) when the operator set one. Pure over the registry + the pane row.
pub fn pane_specialists(db: &Connection, pane: &Value) -> Vec<Value> {
    let reg = crate::constitution_constitution_agents::constitutionAgents(&json!({}), db).unwrap_or(Value::Null);
    let all: Vec<Value> = reg.get("agents").and_then(Value::as_array).cloned().unwrap_or_default();
    let allow: Option<Vec<String>> = pane.get("settings_json").and_then(Value::as_str).and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|v| v.get("specialists").and_then(Value::as_array).map(|a| a.iter().map(|x| s(Some(x))).collect()));
    all.into_iter().filter(|a| a.get("ready").and_then(Value::as_bool).unwrap_or(false) && allow.as_ref().map(|l| l.contains(&s(a.get("id")))).unwrap_or(true)).collect()
}

/// A specialist's validated review as markdown for the calling agent. Pure.
pub fn specialist_review_md(r: &Value) -> String {
    let rv = r.get("review").filter(|v| v.is_object());
    let Some(rv) = rv else { return format!("# {} on {}\n\n_the specialist's reply could not be validated{}_", s(r.get("agent")), s(r.get("path")), r.get("parse_excerpt").and_then(Value::as_str).map(|e| format!(": {e}")).unwrap_or_default()); };
    let mut out = vec![format!("# {} on {}", s(r.get("agent")), s(r.get("path")))];
    let sm = rv.get("summary").cloned().unwrap_or(Value::Null);
    out.push(format!("**{}** · {}\n\n{}", s(sm.get("level")), s(sm.get("title")), s(sm.get("headline"))));
    if let Some(t) = rv.get("theory").filter(|v| v.is_object()) {
        out.push(format!("## Theory — {}\n\n{}{}{}", s(t.get("name")), s(t.get("explanation")),
            t.get("root_cause").and_then(Value::as_str).map(|x| format!("\n\nRoot cause: {x}")).unwrap_or_default(),
            t.get("if_nothing_changes").and_then(Value::as_str).map(|x| format!("\n\nIf nothing changes: {x}")).unwrap_or_default()));
    }
    let cards = rv.get("cards").and_then(Value::as_array).cloned().unwrap_or_default();
    if !cards.is_empty() { out.push(format!("## Findings\n\n{}", cards.iter().map(|c| format!("- [{}·{}] **{}** — {}", s(c.get("type")), s(c.get("severity")), s(c.get("title")), s(c.get("body")))).collect::<Vec<_>>().join("\n"))); }
    let actions = rv.get("actions").and_then(Value::as_array).cloned().unwrap_or_default();
    if !actions.is_empty() { out.push(format!("## Actions\n\n{}", actions.iter().map(|a| format!("- **{}** ({} impact · effort {}) — {}", s(a.get("title")), s(a.get("impact")), s(a.get("effort")), s(a.get("strategy")))).collect::<Vec<_>>().join("\n"))); }
    let ev = rv.get("evidence").and_then(Value::as_array).cloned().unwrap_or_default();
    if !ev.is_empty() { out.push(format!("Evidence: {}", ev.iter().map(|e| s(e.get("id"))).collect::<Vec<_>>().join(", "))); }
    if let Some(p) = rv.get("expected_pushback").and_then(Value::as_str) { out.push(format!("Expected pushback: {p}")); }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    #[test] fn a_one_word_opencode_permission_stays_the_rule_for_everything_else() {
        let mut p = permission_map(json!({ "permission": "ask" }).as_object().unwrap()); allow_antiloki_tools(&mut p, true);
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"*":"ask","antiloki_*":"allow"}"#, "the later rule wins in OpenCode, so the order matters");
        let mut mine = Map::from_iter([("antiloki_*".to_string(), json!("deny"))]); allow_antiloki_tools(&mut mine, true); assert_eq!(mine["antiloki_*"], json!("deny")); allow_antiloki_tools(&mut mine, false); assert_eq!(mine["antiloki_*"], json!("deny"));
    }
    #[test]
    fn an_agent_in_the_shared_folder_gets_the_tools_settings_and_nothing_else() {
        let d = std::env::temp_dir().join(format!("antiloki-shared-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join(".cursor")).unwrap();
        std::fs::write(d.join(".cursor/mcp.json"), r#"{"mcpServers":{"mine":{"command":"x"}}}"#).unwrap(); std::fs::write(d.join("opencode.json"), r#"{"model":"m","permission":{"bash":"allow"}}"#).unwrap();
        for kind in ["cursor", "opencode", "claude-code"] { super::wire_shared_folder(&d, kind, 7, true); }
        let read = |f: &str| -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(d.join(f)).unwrap()).unwrap() };
        let c = read(".cursor/mcp.json"); assert!(c["mcpServers"]["mine"].is_object()); assert_eq!(c["mcpServers"]["antiloki"]["env"]["ANTILOKI_PANE_ID"], "7");
        let o = read("opencode.json"); assert_eq!((o["model"].as_str(), o["permission"]["bash"].as_str()), (Some("m"), Some("allow"))); assert!(o["mcp"]["antiloki"].is_object()); assert_eq!(o["permission"]["antiloki_*"], json!("allow"));
        assert!(read(".antiloki/mcp.json")["mcpServers"]["antiloki"].is_object());
        assert!(!d.join("CLAUDE.md").exists() && !d.join("AGENTS.md").exists() && !d.join(".claude").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        let _ = crate::codebase_code_schema::ensureCodeSchema(&c, &json!({}));
        c
    }

    fn temp_worktree(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-bake-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let _ = crate::proc::cmd("git").args(["init", "-q"]).current_dir(&d).output();
        std::fs::write(d.join("AGENTS.md"), "# repo rules\n").unwrap();
        d
    }

    /// CURSOR AND GROK CHAT PANES REACH THE ANTILOKI TOOLS (audit 2026-10-05: only Claude, Codex and OpenCode did, so a front-mode chat on the other seats could not hand work to Heimdall). The server entry is merged
    /// into the project's own MCP file, removed when the pane has no live tools, and a file the project TRACKS is left alone.
    /// 2026-10-13: committed AGENTS.md / seat files stay byte-identical and bake leaves git status clean.
    #[test]
    fn tracked_project_files_are_not_rewritten_in_a_main_checkout() {
        let wt = temp_worktree("hands-off");
        let agents_before = std::fs::read_to_string(wt.join("AGENTS.md")).unwrap();
        std::fs::write(wt.join("opencode.json"), r#"{"model":"x"}"#).unwrap();
        crate::proc::cmd("git").args(["add", "AGENTS.md", "opencode.json"]).current_dir(&wt).output().unwrap();
        crate::proc::cmd("git").args(["commit", "-qm", "seed"]).current_dir(&wt).output().unwrap();
        rewrite_brief(&conn(), &json!({ "pane_kind": "codex", "session_id": 1, "extra_prompt_md": "", "settings_json": "{}" }), &wt, None, false);
        patch_opencode_json(&wt, 1, true, "ask", None, false);
        let status = crate::proc::cmd("git").args(["status", "--porcelain"]).current_dir(&wt).output().unwrap();
        assert!(String::from_utf8_lossy(&status.stdout).trim().is_empty(), "git status must stay clean");
        assert_eq!(std::fs::read_to_string(wt.join("AGENTS.md")).unwrap(), agents_before);
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn a_plain_opencode_pane_gets_an_opencode_json_with_the_antiloki_mcp_and_other_engines_do_not() {
        let wt = temp_worktree("oc-plain");
        patch_opencode_json(&wt, 4, true, "ask", None, false);
        assert!(!wt.join("opencode.json").exists(), "a Claude pane never gets an opencode.json");
        patch_opencode_json(&wt, 4, false, "ask", None, true);
        assert!(!wt.join("opencode.json").exists(), "no live tools, nothing to wire");
        patch_opencode_json(&wt, 4, true, "ask", None, true);
        let c: Value = serde_json::from_str(&std::fs::read_to_string(wt.join("opencode.json")).unwrap()).unwrap();
        assert_eq!(c["mcp"]["antiloki"]["type"], "local", "{c}");
        assert_eq!(c["mcp"]["antiloki"]["environment"]["ANTILOKI_PANE_ID"], "4", "{c}");
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn cursor_and_grok_get_the_antiloki_mcp_in_their_project_files_and_lose_it_again() {
        let wt = temp_worktree("cg-mcp");
        // Cursor: a project server already there is kept
        std::fs::create_dir_all(wt.join(".cursor")).unwrap();
        std::fs::write(wt.join(".cursor/mcp.json"), r#"{"mcpServers":{"mine":{"command":"x"}}}"#).unwrap();
        write_cursor_mcp(&wt, 7, true, None);
        let c: Value = serde_json::from_str(&std::fs::read_to_string(wt.join(".cursor/mcp.json")).unwrap()).unwrap();
        assert!(c["mcpServers"]["mine"].is_object() && c["mcpServers"]["antiloki"]["env"]["ANTILOKI_PANE_ID"] == "7", "{c}");
        write_cursor_mcp(&wt, 7, false, None);
        let c: Value = serde_json::from_str(&std::fs::read_to_string(wt.join(".cursor/mcp.json")).unwrap()).unwrap();
        assert!(c["mcpServers"]["mine"].is_object() && c["mcpServers"].get("antiloki").is_none(), "only ours leaves: {c}");
        // Grok: a marked block appended to the project's own TOML, replaced in place, removed cleanly
        std::fs::create_dir_all(wt.join(".grok")).unwrap();
        std::fs::write(wt.join(".grok/config.toml"), "[ui]\nscreen_mode = \"minimal\"\n").unwrap();
        write_grok_mcp(&wt, 7, true, None); write_grok_mcp(&wt, 8, true, None);
        let t = std::fs::read_to_string(wt.join(".grok/config.toml")).unwrap();
        assert!(t.contains("screen_mode") && t.contains("[mcp_servers.antiloki]") && t.contains("ANTILOKI_PANE_ID = \"8\""), "{t}");
        assert_eq!(t.matches("[mcp_servers.antiloki]").count(), 1, "replaced, never duplicated: {t}");
        write_grok_mcp(&wt, 8, false, None);
        let t = std::fs::read_to_string(wt.join(".grok/config.toml")).unwrap();
        assert!(t.contains("screen_mode") && !t.contains("antiloki"), "{t}");
        // a file the project tracks is never touched
        crate::proc::cmd("git").args(["add", "-f", ".grok/config.toml"]).current_dir(&wt).output().unwrap();
        write_grok_mcp(&wt, 9, true, None);
        assert!(!std::fs::read_to_string(wt.join(".grok/config.toml")).unwrap().contains("antiloki"), "a tracked file stays the project's");
        let _ = std::fs::remove_dir_all(&wt);
    }

    fn seed(c: &Connection, wt: &Path, kind: &str, settings: Option<&str>) -> i64 {
        c.execute("INSERT INTO cockpit_sessions (id, name) VALUES (1, 's')", []).unwrap();
        c.execute(
            "INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, scope_json, worktree_path, pane_kind, extra_prompt_md, settings_json) VALUES (7, 1, 0, 'a', '[\"src/ui/**\"]', ?1, ?2, 'be careful', ?3)",
            rusqlite::params![wt.to_string_lossy().to_string(), kind, settings],
        ).unwrap();
        c.execute("INSERT INTO judgments.review_snapshots (id, created_at, scope_json, manifest_json, constitution_hash, status, engineering_version) VALUES (3,'2026-09-01 00:00:00','{}','[]','h','done','v3')", []).unwrap();
        c.execute(r#"INSERT INTO laws.review_laws (scope, key, title, rule_md, status, enforcement, weight, detector, config_json, applies_json) VALUES ('language','no-var','No var','Use let/const.','ratified','score',5,'regex','{"pattern":"\\bvar\\b"}','{"languages":["js","ts"]}')"#, []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, archetype, loc, axes_json) VALUES (3,'file','src/ui/A.tsx',60,55,'logic',120,'{\"structure\":{\"score\":30,\"parts\":{\"basis\":\"cognitive\",\"worst_block\":\"go\",\"worst_line\":9,\"worst_cognitive\":22,\"max_nesting\":5}}}')", []).unwrap();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, archetype, loc, axes_json) VALUES (3,'file','src/ui/B.tsx',95,95,'logic',20,'{\"structure\":{\"score\":95,\"parts\":{}}}')", []).unwrap();
        c.execute("INSERT INTO judgments.review_violations (snapshot_id, path, law_key, law_scope, weight, severity, confidence, line, evidence) VALUES (3,'src/ui/A.tsx','no-var','project',5,'warn',0.9,4,'var x = 1')", []).unwrap();
        c.execute("INSERT INTO cockpit_blackboard (id, session_id, kind, body_md) VALUES (11, 1, 'context', 'ship the login page')", []).unwrap();
        7
    }

    /// 2026-10-01, the X-ray experiments: `crates/` was excluded from the index, so an agent that trusted the pack never saw the Rust port of the
    /// message it was told to fix. The pack must say where the index stops - in INDEX.md and in the brief - and re-say it when the index changes.
    #[test]
    fn the_pack_says_what_the_index_does_not_cover_instead_of_implying_completeness() {
        let c = conn();
        let wt = temp_worktree("coverage");
        let id = seed(&c, &wt, "claude-code", None);
        std::fs::create_dir_all(wt.join("src/ui")).unwrap();
        std::fs::create_dir_all(wt.join("crates/core/src")).unwrap();
        std::fs::write(wt.join("src/ui/A.tsx"), "export const A = () => { throw new Error('cannot extract names'); };\n").unwrap();
        std::fs::write(wt.join("crates/core/src/extract.rs"), "pub fn f() -> Result<(), String> { Err(\"cannot extract names\".into()) }\n").unwrap();
        let _ = crate::proc::cmd("git").args(["add", "-A"]).current_dir(&wt).output();
        // 2026-10-13: add -A also stages the generated AGENTS.md; a tracked copy is hands-off, and this test still wants the brief written into it.
        let _ = crate::proc::cmd("git").args(["rm", "--cached", "-q", "--ignore-unmatch", "--", "AGENTS.md"]).current_dir(&wt).output();
        let h = crate::codebase_atlas_ingest::hashSource(&std::fs::read_to_string(wt.join("src/ui/A.tsx")).unwrap());
        c.execute("INSERT INTO code_nodes (id, origin, name, path, type, content_hash) VALUES ('src/ui/A.tsx','atlas','A.tsx','src/ui/A.tsx','file',?1)", rusqlite::params![h]).unwrap();
        c.execute("INSERT INTO code_scope (origin, exclude_json, include_json) VALUES ('atlas','[\"crates/**\"]','[]')", []).unwrap();
        c.execute("UPDATE cockpit_panes SET extra_prompt_md = 'fix the message \"cannot extract names\" everywhere' WHERE id = ?1", rusqlite::params![id]).unwrap();

        let r = cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true), "{r}");
        let idx = std::fs::read_to_string(wt.join(CONTEXT_DIR).join("INDEX.md")).unwrap();
        assert!(idx.contains("## Coverage of this intel - read before you trust it"), "{idx}");
        assert!(idx.contains("- NOT INDEXED: `crates/core/src/extract.rs` - excluded from the index by the scope setting `crates/**`"), "{idx}");
        assert!(idx.find("## Coverage").unwrap() < idx.find("## Files").unwrap(), "said before the file list is read");
        let manifest: Value = serde_json::from_str(&std::fs::read_to_string(wt.join(CONTEXT_DIR).join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["coverage"]["gaps"], json!(1));
        for f in ["AGENTS.md", "CLAUDE.md"] {
            let t = std::fs::read_to_string(wt.join(f)).unwrap();
            assert!(t.contains("COVERAGE WARNING: 1 file(s) this ask touches are NOT fully covered by the index (first: crates/core/src/extract.rs)"), "{f}: {t}");
        }
        // unchanged inputs: the bake is a no-op, and says the same thing
        let again = cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert_eq!(again["skipped"], json!(true));
        // the operator widens the index (no more exclusion): the next bake is NOT answered by the old notice
        c.execute("UPDATE code_scope SET exclude_json = '[]' WHERE origin = 'atlas'", []).unwrap();
        let widened = cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert_eq!(widened["skipped"], json!(false), "the coverage is part of the pack's identity");
        let idx2 = std::fs::read_to_string(wt.join(CONTEXT_DIR).join("INDEX.md")).unwrap();
        assert!(idx2.contains("- NOT INDEXED: `crates/core/src/extract.rs` - not in the index"), "no longer 'excluded': it simply was never ingested: {idx2}");
    }

    // ── the agent's craft in the place its seat reads (2026-10-01) ─────────────────────────────────────────────────────────────

    fn seat_conn() -> Connection {
        let c = conn();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge; \
             CREATE TABLE knowledge.skill_library (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, name TEXT NOT NULL, description TEXT NOT NULL, body_md TEXT NOT NULL DEFAULT '', \
               assets_json TEXT NOT NULL DEFAULT '{}', applies_json TEXT NOT NULL DEFAULT '{}', verify_cmd TEXT, enabled INTEGER NOT NULL DEFAULT 1, version INTEGER NOT NULL DEFAULT 1, \
               provenance TEXT NOT NULL DEFAULT 'authored', created_at TEXT, updated_at TEXT); \
             INSERT INTO knowledge.skill_library (slug, name, description, body_md) VALUES ('tdd','TDD','write the test first','# TDD\nRed, green, refactor.'), ('docs','Docs','keep the README true','# Docs\nUpdate it.');").unwrap();
        c
    }
    const HIRED: &str = r#"{"skills":["tdd","docs"],"specialists":["axis:structure"]}"#;

    fn exists(wt: &Path, rel: &str) -> bool { wt.join(rel).exists() }
    fn read(wt: &Path, rel: &str) -> String { std::fs::read_to_string(wt.join(rel)).unwrap_or_else(|_| panic!("{rel} should exist")) }

    #[test]
    fn an_agent_hired_with_skills_and_specialists_gets_them_where_its_seat_reads_them() {
        for (kind, skill_dirs, rules, no_dirs) in [
            ("cursor", vec![".cursor/skills", ".agents/skills"], Some(".cursor/rules/antiloki.mdc"), vec![".claude/skills", ".grok"]),
            ("grok", vec![".grok/skills", ".agents/skills"], Some(".grok/rules/antiloki.md"), vec![".claude/skills", ".cursor"]),
            ("gemini", vec![".agents/skills"], Some(".agents/rules/antiloki.md"), vec![".claude/skills", ".cursor", ".grok"]),
            ("claude-code", vec![".claude/skills", ".agents/skills"], None, vec![".cursor", ".grok", ".agents/rules"]),
            ("codex", vec![".agents/skills"], None, vec![".claude/skills", ".cursor", ".grok", ".agents/rules"]),
        ] {
            let c = seat_conn();
            let wt = temp_worktree(&format!("seat-{kind}"));
            let id = seed(&c, &wt, kind, Some(HIRED));
            let r = cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
            assert_eq!(r["ok"], json!(true), "{kind}: {r}");
            for d in &skill_dirs {
                assert!(read(&wt, &format!("{d}/tdd/SKILL.md")).starts_with("---\nname: TDD\ndescription: write the test first\n---"), "{kind}: {d}");
                assert!(exists(&wt, &format!("{d}/docs/SKILL.md")), "{kind}: {d}");
                assert!(exists(&wt, &format!("{d}/antiloki-context/SKILL.md")), "{kind}: the built-in context skill rides with the pack in {d}");
            }
            for d in &no_dirs { assert!(!exists(&wt, d), "{kind}: {d} is not read by this seat and must not be written"); }
            let agents = read(&wt, "AGENTS.md");
            assert!(agents.contains("- **TDD** — write the test first → `.agents/skills/tdd/SKILL.md`"), "{kind}: the AGENTS.md index every seat reads: {agents}");
            match rules {
                Some(f) => {
                    let t = read(&wt, f);
                    assert!(t.contains("<!-- ANTILOKI-SEAT-RULES -->"), "{kind}: {t}");
                    assert!(t.contains(&format!("- **TDD** - write the test first -> `{}/tdd/SKILL.md`", skill_dirs[0])), "{kind}: the path THIS seat finds the skill at: {t}");
                    assert!(t.contains("- `axis:structure` - Structure Reviewer"), "{kind}: the specialist as a lens: {t}");
                    assert!(t.contains("`.antiloki/context/laws.md`"), "{kind}: where the laws are: {t}");
                    if kind == "cursor" { assert!(t.starts_with("---\ndescription: antiloki") && t.contains("alwaysApply: true"), "Cursor loads a .mdc rule only with its front matter: {t}"); }
                    else { assert!(t.starts_with("<!-- ANTILOKI-SEAT-RULES -->"), "{kind}: {t}"); }
                    let ex = std::fs::read_to_string(wt.join(".git/info/exclude")).unwrap_or_default();
                    assert!(ex.lines().any(|l| l.trim() == f), "{kind}: {f} must never reach a PR: {ex}");
                }
                None => assert!(!exists(&wt, ".cursor/rules/antiloki.mdc") && !exists(&wt, ".grok/rules/antiloki.md") && !exists(&wt, ".agents/rules/antiloki.md"), "{kind} reads CLAUDE.md/AGENTS.md: no rules file"),
            }
            let _ = std::fs::remove_dir_all(&wt);
        }
    }

    #[test]
    fn nothing_leaks_out_of_the_agents_own_worktree() {
        let c = seat_conn();
        let parent = std::env::temp_dir().join(format!("antiloki-bake-leak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let operator = parent.join("operator-repo");
        std::fs::create_dir_all(&operator).unwrap();
        let wt = parent.join("box");
        std::fs::create_dir_all(&wt).unwrap();
        let _ = crate::proc::cmd("git").args(["init", "-q"]).current_dir(&wt).output();
        let id = seed(&c, &wt, "cursor", Some(HIRED));
        cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert!(exists(&wt, ".cursor/skills/tdd/SKILL.md"));
        assert_eq!(std::fs::read_dir(&operator).unwrap().count(), 0, "the operator's own tree is not the agent's box: nothing is written there");
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// 2026-10-13: committed AGENTS.md, CLAUDE.md, opencode.json, mcp.json, seat rules and SKILL.md survive a bake; git status stays empty.
    #[test]
    fn a_bake_leaves_tracked_project_files_and_a_clean_status() {
        let c = seat_conn();
        let wt = temp_worktree("tracked-full");
        for a in [["config", "user.email", "t@t"], ["config", "user.name", "t"]] {
            crate::proc::cmd("git").args(a).current_dir(&wt).output().unwrap();
        }
        let skill = "---\nname: TDD\ndescription: write the test first\n---\n# TDD\nkept bytes\n";
        std::fs::create_dir_all(wt.join(".cursor/skills/tdd")).unwrap();
        std::fs::create_dir_all(wt.join(".cursor/rules")).unwrap();
        std::fs::create_dir_all(wt.join(".antiloki")).unwrap();
        std::fs::write(wt.join("AGENTS.md"), "# repo rules\n").unwrap();
        std::fs::write(wt.join("CLAUDE.md"), "# claude\n").unwrap();
        std::fs::write(wt.join("opencode.json"), "{\"model\":\"x\"}\n").unwrap();
        std::fs::write(wt.join(".antiloki/mcp.json"), "{}\n").unwrap();
        std::fs::write(wt.join(".cursor/rules/antiloki.mdc"), "project rules\n").unwrap();
        std::fs::write(wt.join(".cursor/skills/tdd/SKILL.md"), skill).unwrap();
        crate::proc::cmd("git").args(["add", "-A"]).current_dir(&wt).output().unwrap();
        let committed = crate::proc::cmd("git").args(["commit", "-qm", "seed"]).current_dir(&wt).output().unwrap();
        assert!(committed.status.success(), "{}", String::from_utf8_lossy(&committed.stderr));
        let id = seed(&c, &wt, "cursor", Some(HIRED));
        let r = cockpitPaneContextBake(&json!({ "pane_id": id, "force": true }), &c).unwrap();
        assert_eq!(r["ok"], json!(true), "{r}");
        assert_eq!(std::fs::read_to_string(wt.join(".cursor/skills/tdd/SKILL.md")).unwrap(), skill);
        let status = crate::proc::cmd("git").args(["status", "--porcelain"]).current_dir(&wt).output().unwrap();
        assert!(String::from_utf8_lossy(&status.stdout).trim().is_empty(), "git status must stay clean: {}", String::from_utf8_lossy(&status.stdout));
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn a_pane_hired_with_nothing_gets_nothing_new_and_the_projects_own_files_are_never_touched() {
        let c = seat_conn();
        let wt = temp_worktree("seat-none");
        let id = seed(&c, &wt, "cursor", None);
        cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert!(!exists(&wt, ".cursor/skills") && !exists(&wt, ".cursor/rules"), "no skills or specialists configured: the bake adds no skill or rule files");
        assert!(exists(&wt, ".cursor/mcp.json"), "...but a pane with live tools carries the antiloki MCP server (the one thing Cursor panes were missing)");
        // the project ships its own skill and its own rule of the same name as ours
        std::fs::create_dir_all(wt.join(".cursor/skills/own")).unwrap();
        std::fs::write(wt.join(".cursor/skills/own/SKILL.md"), "the project's own skill").unwrap();
        std::fs::create_dir_all(wt.join(".cursor/rules")).unwrap();
        std::fs::write(wt.join(".cursor/rules/antiloki.mdc"), "the project's own rule, no mark").unwrap();
        c.execute("UPDATE cockpit_panes SET settings_json = ?1 WHERE id = ?2", rusqlite::params![HIRED, id]).unwrap();
        cockpitPaneContextBake(&json!({ "pane_id": id, "force": true }), &c).unwrap();
        assert_eq!(read(&wt, ".cursor/skills/own/SKILL.md"), "the project's own skill");
        assert_eq!(read(&wt, ".cursor/rules/antiloki.mdc"), "the project's own rule, no mark", "a file without our mark is the project's");
        assert!(exists(&wt, ".cursor/skills/tdd/SKILL.md"), "ours are written beside it");
    }

    #[test]
    fn moving_the_pane_to_another_seat_takes_the_old_seats_files_away() {
        let c = seat_conn();
        let wt = temp_worktree("seat-switch");
        let id = seed(&c, &wt, "cursor", Some(HIRED));
        cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert!(exists(&wt, ".cursor/skills/tdd/SKILL.md") && exists(&wt, ".cursor/rules/antiloki.mdc"));
        std::fs::create_dir_all(wt.join(".cursor/skills/own")).unwrap();
        std::fs::write(wt.join(".cursor/skills/own/SKILL.md"), "mine").unwrap();
        c.execute("UPDATE cockpit_panes SET pane_kind = 'grok' WHERE id = ?1", rusqlite::params![id]).unwrap();
        cockpitPaneContextBake(&json!({ "pane_id": id, "force": true }), &c).unwrap();
        assert!(!exists(&wt, ".cursor/skills/tdd") && !exists(&wt, ".cursor/rules/antiloki.mdc"), "Cursor's copies are gone");
        assert!(exists(&wt, ".cursor/skills/own/SKILL.md"), "the project's own skill stays");
        assert!(exists(&wt, ".grok/skills/tdd/SKILL.md") && exists(&wt, ".grok/rules/antiloki.md") && exists(&wt, ".agents/skills/tdd/SKILL.md"));
        // un-hire the skills: the rules file of ours is taken away, not left stale
        c.execute("UPDATE cockpit_panes SET settings_json = '{\"skills\":[],\"specialists\":[]}' WHERE id = ?1", rusqlite::params![id]).unwrap();
        cockpitPaneContextBake(&json!({ "pane_id": id, "force": true }), &c).unwrap();
        assert!(!exists(&wt, ".grok/rules/antiloki.md") && !exists(&wt, ".grok/skills/tdd") && !exists(&wt, ".agents/skills/tdd"), "hired with nothing now: what our earlier start wrote is gone");
    }

    #[test]
    fn bakes_the_default_pack_and_rewrites_the_brief() {
        let c = conn();
        let wt = temp_worktree("default");
        let id = seed(&c, &wt, "claude-code", None);
        let r = cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true), "{r}");
        assert_eq!(r["skipped"], json!(false));
        let files: Vec<String> = r["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
        assert_eq!(files, vec!["laws.md", "folders/src__ui.md", "files/src__ui__A.tsx.md", "graph/src__ui.json", "tests/src__ui.md", "docs/11.md", "INDEX.md", "manifest.json"], "B (95) is not 'needs work'");
        let ctx = wt.join(CONTEXT_DIR);
        assert!(ctx.join("INDEX.md").is_file());
        let a = std::fs::read_to_string(ctx.join("files/src__ui__A.tsx.md")).unwrap();
        assert!(a.contains("| structure | 30 | 0.15 | worst block `go` L9 (cognitive 22, nesting 5) | split `go` (L9)"), "{a}");
        assert!(a.contains("  - L4: `var x = 1`"));
        let idx = std::fs::read_to_string(ctx.join("INDEX.md")).unwrap();
        assert!(idx.contains("from judge snapshot #3 (2026-09-01 00:00, engineering_version v3)"));
        assert!(idx.contains("- `files/src__ui__A.tsx.md` — file review 55/100 (law score 60): weakest structure 30, 1 violations"));
        assert!(idx.contains("- `docs/11.md` — context doc #11 — ship the login page"));
        assert!(idx.contains("## Live tools (MCP server `antiloki`)"));
        let manifest: Value = serde_json::from_str(&std::fs::read_to_string(ctx.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["snapshot_id"], json!(3));
        assert_eq!(manifest["items"][2]["snapshot_id"], json!(3));
        // brief rewritten in BOTH files for claude-code, repo content preserved below
        for f in ["AGENTS.md", "CLAUDE.md"] {
            let t = std::fs::read_to_string(wt.join(f)).unwrap();
            assert!(t.contains("CONTEXT PACK (generated by antiloki at"), "{f}: {t}");
            assert!(t.contains("- .antiloki/context/laws.md — 1 ratified laws"));
            assert!(t.contains("OPERATOR GUIDANCE:\nbe careful"));
            assert!(t.contains("LIVE TOOLS (MCP server `antiloki`)"));
            assert!(!t.contains("--- CONTEXT DOC #11 ---"), "docs are paths now");
        }
        assert!(std::fs::read_to_string(wt.join("AGENTS.md")).unwrap().ends_with("# repo rules\n"));
        // Claude hook wired into the project-local settings layer, merged, never duplicated
        let hook_cfg: Value = serde_json::from_str(&std::fs::read_to_string(wt.join(".claude/settings.local.json")).unwrap()).unwrap();
        let pre = hook_cfg["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert!(pre[0]["hooks"][0]["command"].as_str().unwrap().ends_with("antiloki-agent hook"), "the Rust sidecar, hook subcommand");
        assert_eq!(pre[0]["matcher"], json!("Read|Grep|Glob|Bash|Edit|Write|MultiEdit|NotebookEdit|mcp__antiloki__.*|mcp__antiqa__.*"));
        assert!(hook_cfg["statusLine"]["command"].as_str().unwrap().ends_with("antiloki-agent statusline"), "usage always visible: the status line is wired");
        assert_eq!(hook_cfg["hooks"]["UserPromptSubmit"].as_array().unwrap().len(), 1, "prompt history: every submitted prompt is logged");
        assert_eq!(hook_cfg["hooks"]["Stop"].as_array().unwrap().len(), 1, "answers: the final text of every turn");
        assert_eq!(hook_cfg["hooks"]["PostToolUse"][0]["matcher"], json!(".*"), "the trace of EVERY tool call (the sagas' stack trace), plans and the audit's written/exec lines among them");
        assert_eq!(hook_cfg["hooks"]["PostToolUseFailure"][0]["matcher"], json!(".*"), "a failed tool call is in the trace too");
        assert_eq!(hook_cfg["hooks"]["UserPromptSubmit"][0]["hooks"][0]["timeout"], json!(PROMPT_HOOK_TIMEOUT_S), "the prompt hook may wait for the person to answer the pre-check question");
        assert!(pre[0]["matcher"].as_str().unwrap().contains("Bash"));
        assert_eq!(manifest["scope"], json!(["src/ui/**"]), "the manifest carries the edit whitelist for the hooks");
        // MCP config + git exclude
        let mcp: Value = serde_json::from_str(&std::fs::read_to_string(wt.join(MCP_CONFIG)).unwrap()).unwrap();
        assert_eq!(mcp["mcpServers"]["antiloki"]["env"]["ANTILOKI_PANE_ID"], json!("7"));
        let excl = std::fs::read_to_string(wt.join(".git/info/exclude")).unwrap_or_default();
        assert!(excl.lines().any(|l| l == ".antiloki/"), "{excl}");
        // hash stored → second bake is a no-op; force rewrites
        let pane = crate::db_json::query_rows(&c, "SELECT settings_json FROM cockpit_panes WHERE id = 7", &[]).unwrap();
        let settings: Value = serde_json::from_str(pane[0]["settings_json"].as_str().unwrap()).unwrap();
        assert_eq!(settings["context_pack_hash"], r["hash"]);
        let r2 = cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert_eq!(r2["skipped"], json!(true));
        let r3 = cockpitPaneContextBake(&json!({ "pane_id": id, "force": true }), &c).unwrap();
        assert_eq!(r3["skipped"], json!(false));
        let hook_cfg: Value = serde_json::from_str(&std::fs::read_to_string(wt.join(".claude/settings.local.json")).unwrap()).unwrap();
        assert_eq!(hook_cfg["hooks"]["PreToolUse"].as_array().unwrap().len(), 1, "re-bake replaces, never duplicates");
        // the measurement: what the hook logged
        std::fs::write(wt.join(".antiloki/reads.jsonl"), "{\"kind\":\"pack\",\"path\":\".antiloki/context/INDEX.md\",\"nudged\":null}\n{\"kind\":\"source\",\"path\":\"src/ui/A.tsx\",\"nudged\":\"src/ui/A.tsx\"}\n{\"kind\":\"tool\",\"path\":\"antiloki_check\",\"nudged\":null}\nnot json\n{\"t\":\"1\",\"tool\":\"Write\",\"kind\":\"blocked\",\"path\":\"src/lib/x.ts\"}\n{\"t\":\"2\",\"tool\":\"Edit\",\"kind\":\"blocked\",\"path\":\"src/lib/x.ts\"}\n").unwrap();
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_reads", "args": {} }), &c).unwrap();
        assert_eq!(r["reads"]["pack"], json!(1));
        assert_eq!(r["reads"]["source"], json!(1));
        assert_eq!(r["reads"]["tool"], json!(1));
        assert_eq!(r["reads"]["nudged"], json!(1));
        assert_eq!(r["reads"]["index_read"], json!(true));
        assert!(r["markdown"].as_str().unwrap().contains("pack 1 · source 1 · search 0 · live tools 1"));
        assert_eq!(r["reads"]["blocked"], json!([{ "path": "src/lib/x.ts", "tool": "Edit", "t": "2" }]), "one entry per path, newest wins");
        // usage: no status-line file yet → an honest none, never a guess
        let u = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_usage", "args": {} }), &c).unwrap();
        assert_eq!(u["usage"]["engine"], json!("claude-code"));
        assert_eq!(u["usage"]["source"], json!("none"));
        // the meter is per PANE, so it must be named for the pane this test actually created — it was pinned to
        // `usage-1.json` and stopped being read the day the fixture's id stopped being 1, which left the
        // assertion below measuring `account_limits` instead of the pane's own meter (found 2026-09-28)
        std::fs::write(wt.join(format!(".antiloki/usage-{id}.json")), r#"{"model":"Opus 5","context":{"used_pct":12},"cost_usd":0.42,"limits":[{"label":"5h","used_pct":23.5,"resets_at":1},{"label":"week","used_pct":41.2,"resets_at":2}]}"#).unwrap();
        let u = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_usage", "args": {} }), &c).unwrap();
        assert!(u["markdown"].as_str().unwrap().contains("Opus 5 · context 12% · $0.42 · 5h 24% · week 41%"), "{u}");
        // plans: a captured plan is pending until decided; a revision after the decision makes it pending again
        std::fs::create_dir_all(wt.join(".antiloki/plans")).unwrap();
        std::fs::write(wt.join(".antiloki/plans/otter.md"), "# Ship the login page\n\n1. x\n").unwrap();
        std::fs::write(wt.join(".antiloki/reads.jsonl"), "{\"t\":\"2025-09-16T10:00:00Z\",\"tool\":\"Write\",\"kind\":\"plan\",\"path\":\"/h/.claude/plans/otter.md\",\"file\":\".antiloki/plans/otter.md\"}\n").unwrap();
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_plans", "args": {} }), &c).unwrap();
        assert_eq!(r["plans"][0]["title"], json!("Ship the login page"));
        assert_eq!(r["plans"][0]["status"], json!("pending"));
        assert!(r["plans"][0]["body_md"].as_str().unwrap().contains("1. x"));
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_plan_decide", "args": { "file": ".antiloki/plans/otter.md", "decision": "accepted" } }), &c).unwrap();
        assert_eq!(r["decision"]["decision"], json!("accepted"));
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_reads", "args": {} }), &c).unwrap();
        assert_eq!(r["reads"]["plans"][0]["status"], json!("accepted"), "{r}");
        std::fs::write(wt.join(".antiloki/reads.jsonl"), "{\"t\":\"2025-09-16T10:00:00Z\",\"tool\":\"Write\",\"kind\":\"plan\",\"path\":\"x\",\"file\":\".antiloki/plans/otter.md\"}\n{\"t\":\"2099-01-01T00:00:00Z\",\"tool\":\"Write\",\"kind\":\"plan\",\"path\":\"x\",\"file\":\".antiloki/plans/otter.md\"}\n").unwrap();
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_reads", "args": {} }), &c).unwrap();
        assert_eq!(r["reads"]["plans"][0]["status"], json!("pending"), "revised after the decision → pending again");
        assert!(cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_plan_decide", "args": { "file": "../x", "decision": "accepted" } }), &c).is_err());
        // answers: the Stop hook's log, newest first
        std::fs::write(wt.join(".antiloki/answers.jsonl"), "{\"t\":\"1\",\"text\":\"first\"}\n{\"t\":\"2\",\"text\":\"second\"}\n").unwrap();
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_answers", "args": {} }), &c).unwrap();
        assert_eq!(r["answers"][0]["text"], json!("second"));
        // AUDIT: hook lines (old ones without `op`) + written outcomes, cross-checked against git
        let git = |args: &[&str]| { let _ = crate::proc::cmd("git").args(args).current_dir(&wt).output(); };
        git(&["init", "-q"]); git(&["config", "user.email", "t@t"]); git(&["config", "user.name", "t"]);
        std::fs::write(wt.join("kept.ts"), "1").unwrap(); git(&["add", "."]); git(&["commit", "-qm", "base"]);
        std::fs::write(wt.join("src.ts"), "export const a = 1;").unwrap(); // written by a tool AND in git
        std::fs::write(wt.join("sneaky.ts"), "x").unwrap();                // in git, never a tool write
        std::fs::write(wt.join(".antiloki/reads.jsonl"), concat!(
            "{\"t\":\"2025-09-16T10:00:00Z\",\"tool\":\"Read\",\"kind\":\"source\",\"path\":\"src.ts\"}\n",
            "{\"t\":\"2025-09-16T10:00:01Z\",\"tool\":\"Edit\",\"kind\":\"source\",\"op\":\"write\",\"path\":\"src.ts\"}\n",
            "{\"t\":\"2025-09-16T10:00:02Z\",\"tool\":\"Edit\",\"kind\":\"written\",\"op\":\"write\",\"path\":\"src.ts\",\"sha\":\"abcd\",\"bytes\":19}\n",
            "{\"t\":\"2025-09-16T10:00:03Z\",\"tool\":\"Edit\",\"kind\":\"blocked\",\"op\":\"write\",\"path\":\"../out.ts\"}\n",
            "{\"t\":\"2025-09-16T10:00:04Z\",\"tool\":\"Bash\",\"kind\":\"exec\",\"op\":\"exec\",\"path\":\"npm test\",\"exit\":0}\n",
        )).unwrap();
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_audit", "args": {} }), &c).unwrap();
        let a = &r["audit"];
        assert_eq!(a["counts"], json!({ "reads": 1, "writes": 1, "blocked": 1, "exec": 1, "search": 0, "tool": 0 }), "{a}");
        assert_eq!(a["cross"]["matched"], json!(["src.ts"]));
        assert_eq!(a["cross"]["git_only"], json!(["sneaky.ts"]), "a write that bypassed the tool layer is exposed by git");
        assert_eq!(a["cross"]["hook_only"], json!([]));
        assert_eq!(a["events"][0]["op"], json!("exec"), "newest first");
        assert_eq!(a["events"][4]["op"], json!("read"), "an old line without `op` gets one derived");
        let f = a["files"].as_array().unwrap().iter().find(|f| f["path"] == json!("src.ts")).unwrap();
        assert_eq!((f["reads"].clone(), f["writes"].clone(), f["sha"].clone()), (json!(1), json!(1), json!("abcd")));
        assert_eq!(a["log"]["sha256"].as_str().unwrap().len(), 64);
        assert!(r["markdown"].as_str().unwrap().contains("git only (changed in the box, NEVER seen as a tool write"));
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_audit", "args": { "op": "write" } }), &c).unwrap();
        assert_eq!(r["audit"]["event_total"], json!(3));
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn budget_cut_is_listed_not_silent_and_none_removes() {
        let c = conn();
        let wt = temp_worktree("budget");
        let id = seed(&c, &wt, "codex", Some(r#"{"context_pack":{"preset":"custom","items":[{"kind":"laws"},{"kind":"file","path":"src/ui/A.tsx"},{"kind":"blast","path":"src/ui/A.tsx"}],"budget_chars":700,"live":false}}"#));
        let r = cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true), "{r}");
        let items = r["items"].as_array().unwrap();
        assert_eq!(items[0]["written"], json!(true), "the first item always lands");
        assert!(items.iter().any(|i| i["written"] == json!(false)), "{r}");
        let idx = std::fs::read_to_string(wt.join(CONTEXT_DIR).join("INDEX.md")).unwrap();
        assert!(idx.contains("## Not materialised (budget 700 chars reached)"));
        let t = std::fs::read_to_string(wt.join("AGENTS.md")).unwrap();
        assert!(t.contains("- not materialised (budget):"));
        assert!(!t.contains("LIVE TOOLS"), "live:false → no tool line");
        assert!(!wt.join("CLAUDE.md").exists(), "codex pane writes AGENTS.md only");
        assert!(!wt.join(".claude/settings.local.json").exists(), "the Claude hook is Claude's");
        let codex: Value = serde_json::from_str(&std::fs::read_to_string(wt.join(".codex/hooks.json")).unwrap()).unwrap();
        assert!(codex["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap().ends_with("antiloki-agent hook"), "codex gets the same hook binary through its own hooks.json");
        assert_eq!(codex["hooks"]["UserPromptSubmit"].as_array().unwrap().len(), 1);
        assert!(!wt.join(MCP_CONFIG).exists());

        // switch to none → the directory goes, docs inline again
        c.execute("UPDATE cockpit_panes SET settings_json = '{\"context_pack\":{\"preset\":\"none\"}}' WHERE id = 7", []).unwrap();
        let r = cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert_eq!(r["removed"], json!(true));
        assert!(!wt.join(CONTEXT_DIR).exists());
        let t = std::fs::read_to_string(wt.join("AGENTS.md")).unwrap();
        assert!(t.contains("--- CONTEXT DOC #11 ---\nship the login page"));
        assert!(!t.contains("CONTEXT PACK"));
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn pending_worktree_and_unknown_pane() {
        let c = conn();
        assert!(cockpitPaneContextBake(&json!({}), &c).is_err());
        assert_eq!(cockpitPaneContextBake(&json!({ "pane_id": 99 }), &c).unwrap()["ok"], json!(false));
        c.execute("INSERT INTO cockpit_sessions (id, name) VALUES (1, 's')", []).unwrap();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, pane_kind) VALUES (7, 1, 0, 'claude-code')", []).unwrap();
        let r = cockpitPaneContextBake(&json!({ "pane_id": 7 }), &c).unwrap();
        assert_eq!(r["pending"], json!(true));
        let r = cockpitPaneContextRefresh(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "baked": 0, "skipped": 0, "pending": 0, "failed": [] }), "pending panes are not provisioned → not refreshed");
        c.execute("UPDATE cockpit_panes SET worktree_path = '/nonexistent/box' WHERE id = 7", []).unwrap();
        let r = cockpitPaneContextRefresh(&json!({}), &c).unwrap();
        assert_eq!(r["pending"], json!(1), "a box gone from disk is pending, not failed: {r}");
        assert_eq!(r["failed"], json!([]));
    }

    #[test]
    fn tool_endpoint_renders_and_rejects_escapes() {
        let c = conn();
        let wt = temp_worktree("tool");
        let id = seed(&c, &wt, "claude-code", None);
        std::fs::create_dir_all(wt.join("src/ui")).unwrap();
        std::fs::write(wt.join("src/ui/A.tsx"), "let x = 1;\n").unwrap();
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_file_review", "args": { "path": "src/ui/A.tsx" } }), &c).unwrap();
        assert!(r["markdown"].as_str().unwrap().contains("| structure | 30 |"), "{r}");
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_folder_review", "args": { "path": "src/ui" } }), &c).unwrap();
        assert!(r["markdown"].as_str().unwrap().contains("# src/ui/"), "{r}");
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_check", "args": { "path": "src/ui/A.tsx" } }), &c).unwrap();
        assert!(r["markdown"].as_str().unwrap().contains("# Check: src/ui/A.tsx"), "{r}");
        assert!(r["markdown"].as_str().unwrap().contains("fixed `no-var`"), "the on-disk content has no `var` any more: {r}");
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_laws", "args": {} }), &c).unwrap();
        assert!(r["markdown"].as_str().unwrap().contains("`no-var`"));
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_context_index", "args": {} }), &c).unwrap();
        assert!(r["markdown"].as_str().unwrap().contains("no context pack baked"));
        cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_context_index", "args": {} }), &c).unwrap();
        assert!(r["markdown"].as_str().unwrap().contains("# Your context pack"));
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_sources", "args": {} }), &c).unwrap();
        assert!(r["sources"].as_array().unwrap().len() > 30);
        assert!(r["markdown"].as_str().unwrap().contains("`file_history`"));
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_query", "args": { "source": "ast", "args": { "path": "src/ui/A.tsx" } } }), &c).unwrap();
        assert_eq!(r["source"], json!("ast"));
        assert!(r["markdown"].as_str().unwrap().contains("# AST of src/ui/A.tsx"), "{r}");
        let r = cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_query", "args": { "source": "laws" } }), &c).unwrap();
        assert!(r["markdown"].as_str().unwrap().contains("`no-var`"));
        assert!(cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_query", "args": { "source": "bogus" } }), &c).is_err());
        assert!(cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "antiloki_check", "args": { "path": "../etc/passwd" } }), &c).is_err());
        assert!(cockpitPaneContextTool(&json!({ "pane_id": id, "tool": "nope", "args": {} }), &c).is_err());
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn any_catalog_source_can_be_a_pack_item() {
        let c = conn();
        let wt = temp_worktree("catalog");
        std::fs::create_dir_all(wt.join("src/ui")).unwrap();
        std::fs::write(wt.join("src/ui/A.tsx"), "export function go(n: number) { return n; }\n").unwrap();
        let id = seed(&c, &wt, "claude-code", Some(r#"{"context_pack":{"preset":"custom","items":[{"kind":"ast","path":"src/ui/A.tsx","args":{"symbol":"go"}},{"kind":"git"},{"kind":"file_history","path":"src/ui/A.tsx"},{"kind":"smells"}],"live":false}}"#));
        let r = cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true), "{r}");
        let files: Vec<String> = r["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
        assert_eq!(files[0], "ast/src__ui__A.tsx__symbol-go.md");
        assert_eq!(files[1], "git/all.md");
        assert_eq!(files[2], "file_history/src__ui__A.tsx.md");
        assert_eq!(files[3], "smells/all.md");
        let ast = std::fs::read_to_string(wt.join(CONTEXT_DIR).join(&files[0])).unwrap();
        assert!(ast.contains("(function_declaration"), "{ast}");
        for f in &files { assert!(wt.join(CONTEXT_DIR).join(f).is_file(), "{f} written even when its source has nothing (honest stub)"); }
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn the_shell_policy_reads_the_pane_settings() {
        assert_eq!(opencode_bash_policy(None), "ask");
        assert_eq!(opencode_bash_policy(Some(r#"{"shell":"allow"}"#)), "allow");
        assert_eq!(opencode_bash_policy(Some(r#"{"shell":"nope"}"#)), "ask");
    }
    #[test]
    fn min_is_read_from_the_settings_and_written_into_the_index_and_the_plugin() {
        assert_eq!(pane_mode(None), ""); assert_eq!(pane_mode(Some(r#"{"min":true}"#)), "min"); assert_eq!(pane_mode(Some(r#"{"max":true}"#)), "max");
        assert_eq!(pane_mode(Some(r#"{"min":true,"max":true}"#)), "min", "min wins a contradiction — the cheaper one");
        let rule = min_rule_md(&["src/ui/**".to_string()]);
        assert!(rule.starts_with("This pane is MIN") && rule.contains("inside your scope (src/ui/**)") && rule.contains("no intel for <path>"));
        let pack = json!({ "preset": "scope", "default_rule": "x", "budget_chars": 1000, "live": true });
        let snap = json!({ "id": 3, "created_at": "2026-09-21T00:00:00Z", "engineering_version": 1 });
        let min = index_md(&pack, &json!({ "generated": "g", "hash": "h", "mode": "min", "scope": ["src/ui/**"], "items": [] }), &snap, "abc", "");
        assert!(min.contains("## MIN mode — exploration is off") && min.contains("This pane is MIN"), "{min}");
        let i = min.find("## MIN mode").unwrap(); let j = min.find("## Files").unwrap(); assert!(i < j, "the rule comes before the file list");
        let normal = index_md(&pack, &json!({ "generated": "g", "hash": "h", "mode": "", "scope": [], "items": [] }), &snap, "abc", "");
        assert!(!normal.contains("MIN mode"));
        // the plugin refuses by the manifest's mode, and says the same thing the hook says
        assert!(OPENCODE_PLUGIN_JS.contains(r#"manifest().mode === "min""#) && OPENCODE_PLUGIN_JS.contains("antiloki MIN: exploration is off") && OPENCODE_PLUGIN_JS.contains(r#"["rg", "grep", "egrep", "fgrep", "ag", "find", "fd", "ls", "tree"]"#));
    }
    #[test]
    fn opencode_json_learns_instructions_and_mcp() {
        let c = conn();
        let wt = temp_worktree("opencode");
        let id = seed(&c, &wt, "opencode", None);
        std::fs::write(wt.join("opencode.json"), r#"{"model":"antiloki-proxy/x","permission":{"edit":"allow"}}"#).unwrap();
        cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        let cfg: Value = serde_json::from_str(&std::fs::read_to_string(wt.join("opencode.json")).unwrap()).unwrap();
        assert_eq!(cfg["instructions"], json!([".antiloki/context/INDEX.md"]));
        assert_eq!(cfg["mcp"]["antiloki"]["type"], json!("local"));
        assert!(cfg["mcp"]["antiloki"]["command"][0].as_str().unwrap().ends_with("antiloki-agent"));
        assert_eq!(cfg["mcp"]["antiloki"]["command"][1], json!("mcp"));
        assert_eq!(cfg["model"], json!("antiloki-proxy/x"), "the rest of the file is untouched");
        let plugin = std::fs::read_to_string(wt.join(".opencode/plugins/antiloki-context.js")).unwrap();
        assert!(plugin.contains("\"tool.execute.before\"") && plugin.contains("\"tool.execute.after\""), "opencode gets the in-process plugin");
        assert!(plugin.contains("export default {") && plugin.contains("id: \"antiloki-context\"") && plugin.contains("ctx.tool.hook(\"execute.before\"") && !plugin.contains("export const"), "the shape OpenCode 2 loads");
        let excl = std::fs::read_to_string(wt.join(".git/info/exclude")).unwrap_or_default();
        assert!(excl.contains(".opencode/plugins/antiloki-context.js"));
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn specialists_follow_the_pane_whitelist_and_render_a_review() {
        let c = conn();
        let all = pane_specialists(&c, &json!({ "settings_json": "{}" }));
        assert!(all.len() > 5, "the registry's ready specialists");
        let first = s(all[0].get("id"));
        let one = pane_specialists(&c, &json!({ "settings_json": format!("{{\"specialists\":[\"{first}\"]}}") }));
        assert_eq!(one.iter().map(|a| s(a.get("id"))).collect::<Vec<_>>(), vec![first.clone()]);
        assert!(pane_specialists(&c, &json!({ "settings_json": "{\"specialists\":[]}" })).is_empty(), "an empty list makes none available");
        let r = cockpitPaneContextTool(&json!({ "pane_id": 0, "tool": "antiloki_specialist", "args": { "specialist": "nope:x", "path": "src/a.ts" } }), &c).unwrap();
        assert_eq!(r["ok"], json!(false)); assert!(s(r.get("error")).contains("not available"));
        let md = specialist_review_md(&json!({ "agent": "Defects Reviewer", "path": "src/a.ts", "review": { "summary": { "level": "finding", "title": "t", "headline": "h" }, "theory": { "name": "n", "explanation": "e" }, "cards": [{ "type": "risk", "severity": "warn", "title": "c", "body": "b" }], "actions": [{ "title": "a", "impact": "small", "effort": "s", "strategy": "st" }], "evidence": [{ "id": "LAW-1" }] } }));
        assert!(md.starts_with("# Defects Reviewer on src/a.ts")); assert!(md.contains("## Theory — n")); assert!(md.contains("- [risk·warn] **c** — b")); assert!(md.contains("Evidence: LAW-1"));
        assert!(specialist_review_md(&json!({ "agent": "X", "path": "p", "review": null, "parse_excerpt": "junk" })).contains("could not be validated: junk"));
    }

    /// RFC 0001: the Heimdall menu's antiQA button (`settings.antiqa.enabled`) puts a second MCP server beside antiloki's, in every engine config,
    /// tells the agent what it is for, and takes both away again when the button goes off. Only OUR entry leaves a project's own file.
    #[test]
    fn the_antiqa_button_adds_a_second_mcp_server_and_the_brief_line_and_off_removes_them() {
        std::env::set_var("ANTIQA_BIN", "/opt/antiqa/antiqa");
        let c = conn();
        let wt = temp_worktree("antiqa");
        let id = seed(&c, &wt, "claude-code", Some(r#"{"antiqa":{"enabled":true}}"#));
        std::fs::create_dir_all(wt.join("src/ui")).unwrap();
        std::fs::write(wt.join("src/ui/A.tsx"), "export const A = 1;\n").unwrap();
        cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        let mcp: Value = serde_json::from_str(&read(&wt, MCP_CONFIG)).unwrap();
        assert_eq!(mcp["mcpServers"]["antiloki"]["env"]["ANTILOKI_PANE_ID"], json!("7"), "antiloki is still there");
        let q = &mcp["mcpServers"]["antiqa"];
        assert_eq!(q["command"], json!("/opt/antiqa/antiqa"));
        assert_eq!(q["args"][0], json!("mcp"));
        assert!(q["args"].as_array().unwrap().iter().any(|a| a.as_str().map_or(false, |a| a.ends_with(".antiloki/qa-reports"))), "runs live inside the pane's own box: {q}");
        assert_eq!(q["env"]["ANTIQA_ORIGIN"], json!("antiloki:pane-7"), "antiQA records who asked");
        assert!(q["env"]["QAA_PACKS_DIR"].as_str().unwrap().ends_with(".antiqa/packs"), "the project's own stories live with the project");
        assert!(read(&wt, "AGENTS.md").contains("ANTIQA (MCP server `antiqa`"), "the agent is told what it is for");
        assert!(read(&wt, "CLAUDE.md").contains("antiqa_list_stories"));
        // the button goes off: the same bake (hash-skip path) takes it away from the config AND the brief
        c.execute("UPDATE cockpit_panes SET settings_json = ?1 WHERE id = 7", rusqlite::params![r#"{"antiqa":{"enabled":false}}"#]).unwrap();
        cockpitPaneContextBake(&json!({ "pane_id": id }), &c).unwrap();
        let mcp: Value = serde_json::from_str(&read(&wt, MCP_CONFIG)).unwrap();
        assert!(mcp["mcpServers"].get("antiqa").is_none() && mcp["mcpServers"].get("antiloki").is_some(), "{mcp}");
        assert!(!read(&wt, "AGENTS.md").contains("ANTIQA (MCP server"), "and the brief stops mentioning it");
    }

    #[test]
    fn antiqa_is_left_out_when_the_button_is_off_or_the_program_is_not_installed() {
        let pane = |settings: &str| json!({ "id": 7, "settings_json": settings });
        std::env::set_var("ANTIQA_BIN", "/opt/antiqa/antiqa");
        assert!(antiqa_server_entry(&pane("{}"), 7, "/w").is_none(), "no setting, no server");
        assert!(antiqa_server_entry(&pane(r#"{"antiqa":{"enabled":false}}"#), 7, "/w").is_none());
        assert_eq!(antiqa_server_entry(&pane(r#"{"antiqa":{"enabled":true,"packs":"/p/qa"}}"#), 7, "/w").unwrap()["env"]["QAA_PACKS_DIR"], json!("/p/qa"), "a pane can point at its own packs");
    }

    #[test]
    fn antiqa_is_found_from_the_override_then_the_path_and_otherwise_not_at_all() {
        let on_path = || Some(PathBuf::from("/usr/bin/antiqa"));
        assert_eq!(antiqa_bin_from(Some("/opt/q".into()), on_path).as_deref(), Some("/opt/q"), "the override wins");
        assert_eq!(antiqa_bin_from(Some("  ".into()), on_path).as_deref(), Some("/usr/bin/antiqa"), "a blank override is no override");
        assert_eq!(antiqa_bin_from(None, on_path).as_deref(), Some("/usr/bin/antiqa"));
        // an entry for a program that is not there would only make the CLI report a dead server
        assert_eq!(antiqa_bin_from(None, || None), None);
    }

    #[test]
    fn cursor_and_grok_and_opencode_carry_antiqa_beside_antiloki_and_only_ours_leaves() {
        let a = json!({ "command": "/opt/antiqa/antiqa", "args": ["mcp"], "env": { "ANTIQA_ORIGIN": "antiloki:pane-7" } });
        let wt = temp_worktree("antiqa-seats");
        std::fs::create_dir_all(wt.join(".cursor")).unwrap();
        std::fs::write(wt.join(".cursor/mcp.json"), r#"{"mcpServers":{"mine":{"command":"x"}}}"#).unwrap();
        write_cursor_mcp(&wt, 7, true, Some(&a));
        let cfg: Value = serde_json::from_str(&read(&wt, ".cursor/mcp.json")).unwrap();
        assert!(cfg["mcpServers"]["mine"].is_object() && cfg["mcpServers"]["antiloki"].is_object() && cfg["mcpServers"]["antiqa"]["command"] == json!("/opt/antiqa/antiqa"), "{cfg}");
        write_cursor_mcp(&wt, 7, true, None);
        let cfg: Value = serde_json::from_str(&read(&wt, ".cursor/mcp.json")).unwrap();
        assert!(cfg["mcpServers"].get("antiqa").is_none() && cfg["mcpServers"]["antiloki"].is_object(), "button off: antiqa leaves, antiloki stays: {cfg}");
        write_cursor_mcp(&wt, 7, false, None);
        let cfg: Value = serde_json::from_str(&read(&wt, ".cursor/mcp.json")).unwrap();
        assert!(cfg["mcpServers"]["mine"].is_object() && cfg["mcpServers"].get("antiloki").is_none() && cfg["mcpServers"].get("antiqa").is_none(), "the project's own server is never touched: {cfg}");
        let t = grok_mcp_block_with(7, "/w", Some(&a));
        assert!(t.contains("[mcp_servers.antiloki]") && t.contains("[mcp_servers.antiqa]") && t.contains("ANTIQA_ORIGIN = \"antiloki:pane-7\""), "{t}");
        assert!(!grok_mcp_block(7, "/w").contains("antiqa"), "the old entry point is unchanged");
        assert_eq!(t.matches(GROK_BEGIN).count(), 1, "one marked block, so replacing it never duplicates");
    }

    /// The button is a pane/update: flipping `settings.antiqa` must re-wire the agent on that very call (and report whether the program was
    /// found), while an unrelated settings write (layout, chips) must leave the box alone.
    #[test]
    fn flipping_the_antiqa_setting_through_pane_update_rewires_the_agent_and_other_settings_do_not() {
        std::env::set_var("ANTIQA_BIN", "/opt/antiqa/antiqa");
        let c = conn();
        let wt = temp_worktree("antiqa-update");
        let id = seed(&c, &wt, "claude-code", Some("{}"));
        std::fs::create_dir_all(wt.join("src/ui")).unwrap();
        std::fs::write(wt.join("src/ui/A.tsx"), "export const A = 1;\n").unwrap();
        let update = |settings: Value| crate::cockpit_cockpit_pane_update::cockpitPaneUpdate(&json!({ "pane_id": id, "settings": settings }), &c).unwrap();
        let on = update(json!({ "antiqa": { "enabled": true } }));
        assert_eq!(on["context_pack"]["antiqa_server"]["command"], json!("/opt/antiqa/antiqa"), "the answer says antiQA was found: {on}");
        let mcp: Value = serde_json::from_str(&read(&wt, MCP_CONFIG)).unwrap();
        assert!(mcp["mcpServers"]["antiqa"].is_object() && mcp["mcpServers"]["antiloki"].is_object(), "{mcp}");
        let layout = update(json!({ "antiqa": { "enabled": true }, "x": 40, "y": 12 }));
        assert!(layout["context_pack"].is_null(), "a layout write touches nothing: {layout}");
        let off = update(json!({ "antiqa": { "enabled": false } }));
        assert!(off["context_pack"]["antiqa_server"].is_null(), "{off}");
        let mcp: Value = serde_json::from_str(&read(&wt, MCP_CONFIG)).unwrap();
        assert!(mcp["mcpServers"].get("antiqa").is_none(), "{mcp}");
    }
}
