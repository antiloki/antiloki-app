//! THE HEADLESS CLI RUNNER — one coding CLI (`claude -p`, `codex exec`, `opencode run`) run once
//! in a directory with a prompt on stdin, answered with its parsed text, tokens, cost and the tools
//! it ran. The goal runs and the temporary subagents are built on it; antiChat's `cli:<engine>`
//! completions can be too. A port of antiloki-BE/src/chat/cli-llm.ts (the runner half).
//!
//! Pure helpers (prompt folding, output parsing, command shape) are tested below; only `run_cli`
//! spawns — with tokio, so a long task never blocks the server.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
/// THE FIVE FRONTIER SEATS PLUS THE GATEWAY (operator 2026-09-27: "we're gonna merge Claude, Codex, Grok, Gemini and
/// Cursor … we already have a lot of data on Chinese models"). Claude, Codex, Gemini, Grok and Cursor are all flat
/// monthly SUBSCRIPTIONS — a merge between them costs nothing per task, which is the product's whole claim. OpenCode
/// is not a seat but a gateway (opencode.ai/zen), billed as metered credit, and it is off by default: see
/// `zen_enabled`.
pub enum Engine { ClaudeCode, Codex, Opencode, Gemini, Grok, Cursor }
impl Engine {
    pub fn key(self) -> &'static str {
        match self {
            Engine::ClaudeCode => "claude-code", Engine::Codex => "codex", Engine::Opencode => "opencode",
            Engine::Gemini => "gemini", Engine::Grok => "grok", Engine::Cursor => "cursor",
        }
    }
    /// Every engine, in the order the router should prefer when it has a free choice (cheapest seat first).
    pub fn all() -> [Engine; 6] { [Engine::Gemini, Engine::ClaudeCode, Engine::Codex, Engine::Grok, Engine::Cursor, Engine::Opencode] }
    /// The binary that reaches this seat. `cursor-agent`, not `cursor` — `cursor` is the editor.
    pub fn bin(self) -> &'static str { self.bins()[0] }
    /// EVERY BINARY THAT CAN REACH THIS SEAT, most current first.
    ///
    /// GEMINI HAS TWO, AND THE OLD ONE IS DEAD FOR SUBSCRIBERS (researched 2026-09-27). Google stopped serving
    /// Gemini CLI for Google AI Pro, Ultra and free accounts on 18 June 2026 and replaced it with Antigravity CLI
    /// (`agy`), which is why an interactive `gemini` login now tells the operator to use Antigravity. The legacy
    /// `gemini` binary still answers, but only against a PAID API KEY — metered per token, which is the billing
    /// shape this product exists to avoid. So `agy` is preferred and `gemini` is the fallback.
    pub fn bins(self) -> &'static [&'static str] {
        match self {
            Engine::ClaudeCode => &["claude"], Engine::Codex => &["codex"], Engine::Opencode => &["opencode"],
            Engine::Gemini => &["agy", "gemini"], Engine::Grok => &["grok"], Engine::Cursor => &["cursor-agent"],
        }
    }
    /// The binary actually present for this seat — `None` when none is installed.
    pub fn bin_present(self) -> Option<&'static str> {
        #[cfg(test)] { return Some(self.bin()); }
        #[cfg(not(test))] { self.bins().iter().copied().find(|b| crate::proc::which(b).is_some()) }
    }
    /// A SEAT IS A SUBSCRIPTION; A GATEWAY IS A BALANCE (2026-09-27). Only OpenCode bills per token from a credit
    /// pool that can empty mid-run — which it did, returning HTTP 402 on twelve consecutive bench runs.
    pub fn is_metered_gateway(self) -> bool { self == Engine::Opencode }
    /// Grok cannot take its prompt on stdin (`os error 6` — it opens a tty); it needs `--prompt-file`. Writing the
    /// prompt to a temp file also keeps it out of argv, which is the standing rule for every engine here.
    pub fn needs_prompt_file(self) -> bool { self == Engine::Grok }
}

/// `cli:<engine>` or the bare engine key → the engine.
pub fn engine_of(model: &Value) -> Option<Engine> {
    let m = model.as_str().unwrap_or("").trim_start_matches("cli:").trim();
    match m {
        "claude" | "claude-code" => Some(Engine::ClaudeCode),
        "codex" => Some(Engine::Codex),
        "opencode" | "opencode-direct" => Some(Engine::Opencode),
        "gemini" | "gemini-cli" => Some(Engine::Gemini),
        "grok" | "xai" => Some(Engine::Grok),
        "cursor" | "cursor-agent" => Some(Engine::Cursor),
        _ => None,
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage { pub role: String, pub content: String }

/// system messages become the system prompt; the rest fold into one transcript the CLI answers.
pub fn fold_messages(messages: &[ChatMessage], wants_json: bool) -> (String, String) {
    let system = messages.iter().filter(|m| m.role == "system").map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n\n").trim().to_string();
    let rest: Vec<&ChatMessage> = messages.iter().filter(|m| m.role != "system").collect();
    let prompt = if rest.len() == 1 && rest[0].role == "user" { rest[0].content.clone() }
        else { rest.iter().map(|m| format!("{}:\n{}", if m.role == "assistant" { "ASSISTANT" } else { "USER" }, m.content)).collect::<Vec<_>>().join("\n\n") };
    let json_rule = if wants_json { "\n\nRespond with a single JSON object only — no prose before or after it, no code fence." } else { "" };
    (system, format!("{prompt}{json_rule}"))
}

/// A TAP ON A CALL'S TOOL USE, LIVE (the operator 2026-11-15: the node shows in real time which tool the agent is using): called with (tool, target) as each tool_use line streams out of the CLI, before the call ends.
pub type LiveTap = Arc<dyn Fn(&str, &str) + Send + Sync>;
/// A TAP ON EVERY STREAMED LINE (2026-11-21: the Editor view records what the agents write): called with each stdout line of a streaming CLI as it arrives. Cheap: callers ignore the lines that are not theirs.
pub type LineTap = Arc<dyn Fn(&str) + Send + Sync>;
/// The tools a streamed stdout line names (Claude Code's stream-json: an `assistant` event with `tool_use` items): (tool, target). Pure.
pub fn live_tools_of_line(line: &str) -> Vec<(String, String)> {
    let l = line.trim(); if !l.starts_with('{') || !l.contains("tool_use") { return vec![]; } let Ok(e) = serde_json::from_str::<Value>(l) else { return vec![] };
    if e.get("type").and_then(Value::as_str) != Some("assistant") { return vec![]; }
    e.get("message").and_then(|m| m.get("content")).and_then(Value::as_array).map(|a| a.iter().filter(|c| c.get("type").and_then(Value::as_str) == Some("tool_use")).map(|c| (c.get("name").and_then(Value::as_str).unwrap_or("tool").to_string(), tool_target(c.get("input")))).collect()).unwrap_or_default()
}
/// A tool's name as the CLI takes it, or a web page rule for one site: nothing that could add a second rule. Pure.
pub fn tool_name_ok(x: &str) -> bool {
    if let Some(h) = x.strip_prefix("WebFetch(domain:").and_then(|r| r.strip_suffix(')')) { return !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'); }
    !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
#[derive(Default, Clone)]
pub struct RunOpts { pub write: bool, /** the operating system's wall around this call (a group member on a CLI with no pre-write hook) */ pub wall: Option<crate::wall::Wall>, /** the most one call may spend (Claude Code only): `--max-budget-usd` */ pub max_budget_usd: Option<f64>, /** tools refused whatever any settings allow (Claude Code only) */ pub disallowed: Vec<String>, /** a settings file for this call (Claude Code only): `--settings` */ pub settings: Option<String>, /** only the MCP servers of `mcp_config`, none of the person's (Claude Code only) */ pub strict_mcp: bool, /** every streamed stdout line (see `LineTap`) */ pub line_tap: Option<LineTap>, /** the live tap (see `LiveTap`) */ pub live: Option<LiveTap>, /** a JSON file of MCP servers for this call (Claude Code only): `--mcp-config` */ pub mcp_config: Option<String>, /** more tools the run may use (Claude Code only), by the CLI's own names: a team agent that may search the web gets WebSearch and WebFetch */ pub extra_tools: Vec<String>, pub max_turns: Option<u32>, pub timeout_ms: Option<u64>, pub env: HashMap<String, String>, pub last_message_file: Option<String>,
    /// where the prompt was written for an engine that cannot read stdin (Grok) — set by `run_cli_inner`, never by a caller
    pub prompt_file: Option<String>,
    /// THE GOAL RUN THIS CALL BELONGS TO (0 = none): set by the run's job runners, written into the audit row, so a run's cost is a sum and not a guess from a time window (2026-10-09: `ai_calls.run_id` was 0 on every row)
    pub run_id: i64,
    /// THE PROMPT ITSELF, for the one engine that takes it in argv (Antigravity's `agy`). Set by `run_cli_inner`, never
    /// by a caller; empty for every other engine, which read it from stdin or a file.
    pub prompt_argv: Option<String>,
    /// THE ROLE'S MODEL (2026-09-22): set from `role_binding`, it wins over the tier — the operator chose it by name
    pub model: Option<String>, pub tier: Option<Tier>,
    /// the prompt-catalog base this call runs under (prompts.rs) — when set, the finished call is
    /// recorded in `brain.ai_calls` so Statistics and Settings › Prompts count it
    pub template: Option<String>,
    /// IMAGES the judge must LOOK at (visual QA, 2026-09-30): absolute paths of screenshots. Claude, Cursor, Gemini and Grok open the files named in the prompt with
    /// their own file tool; Codex cannot view a file that way, so its images travel as `-i <path>` (probed: `-i` swallows a following positional prompt, hence stdin)
    pub images: Vec<String>,
    /// WHERE A CLAUDE ONE-SHOT WRITES ITS PLAN WINDOWS (2026-10-11): a headless `claude -p` has no status line, but its stream carries the account's `rate_limit_event`. Set only by
    /// the first-run seat check, so the switcher shows the week's usage before the user has spent anything through antiloki; `<root>/.antiloki/usage-0.json`, the file the meter reader scans.
    pub meter_root: Option<String>,
    /// Per-team call ledger context (RFC 0011 §6); when set, `run_cli` appends a row to `bench.sqlite`.
    pub ledger: Option<crate::ledger::Ctx> }

/// THE TIERS ON THE SUBSCRIPTIONS (operator 2026-09-19: "which tier is required for the task; how many
/// sub-agents and their tiers; the review and consolidation — on claude code, codex and opencode"). A tier
/// is an engine's model/effort switch: low = the fast model, high = the strongest with the most effort.
/// `tier_model` is the table; `ANTILOKI_TIER_MODEL_<ENGINE>_<TIER>` (e.g. ANTILOKI_TIER_MODEL_CODEX_HIGH)
/// overrides one cell. An empty cell means "the engine's own default" (no flag).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Tier { Low, #[default] Mid, High }
impl Tier {
    pub fn key(self) -> &'static str { match self { Tier::Low => "low", Tier::Mid => "mid", Tier::High => "high" } }
    pub fn of(s: &str) -> Option<Tier> { match s.trim().to_ascii_lowercase().as_str() { "low" => Some(Tier::Low), "mid" | "medium" => Some(Tier::Mid), "high" => Some(Tier::High), _ => None } }
    pub fn up(self) -> Tier { match self { Tier::Low => Tier::Mid, _ => Tier::High } }
}
/// THE ROLES (PLAN-THE-DIAL, 2026-09-22): what a call IS, which is what the operator actually chooses a model for.
/// A tier said "how strong" and leaked (`low` and `mid` were both sonnet until this morning); a role says "the thing
/// that writes the contract", "the thing that fills a stub", "the thing that checks one claim" — and each binds to an
/// engine and a model the operator picked on the Costs page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { Planner, Worker, Qa }
impl Role {
    pub fn key(self) -> &'static str { match self { Role::Planner => "planner", Role::Worker => "worker", Role::Qa => "qa" } }
    pub fn of(s: &str) -> Option<Role> { match s.trim().to_ascii_lowercase().as_str() { "planner" => Some(Role::Planner), "worker" => Some(Role::Worker), "qa" => Some(Role::Qa), _ => None } }
    /// what a role falls back to while nothing is bound: the tier it used before roles existed
    pub fn legacy_tier(self) -> Tier { match self { Role::Planner => Tier::Mid, Role::Worker => Tier::Low, Role::Qa => Tier::High } }
}
/// THE BINDING a role runs under: the engine and, when the operator named one, the model. `None` model = the engine's
/// own choice for the role's legacy tier, so an unconfigured install behaves exactly as it did before.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding { pub engine: Engine, pub model: Option<String>, pub source: &'static str }
/// A RECORD KEY (`engine:model`) AS A BINDING — strictly, or not at all.
///
/// The merge record stores winners as keys. A key is a label; a `Binding` is a thing that starts. Turning one into
/// the other is where SWE-bench went wrong on 2026-09-26 — it filed `antiloki-opencode-qwen3.6-plus` as a winning
/// worker, which the router could have chosen and then failed to launch. So: `raw` and `-` are not engines, an
/// engine this build does not know is not guessed at, and anything unparseable answers None. Pure — tested.
pub fn binding_of_key(key: &str) -> Option<(Engine, String)> {
    let key = key.trim();
    if key.is_empty() || key == "raw" || key == "-" { return None }
    let (e, m) = key.split_once(':')?;
    let engine = engine_of(&json!(e.trim()))?;
    let m = m.trim();
    if m.is_empty() { return None }
    Some((engine, m.to_string()))
}

/// THE SEATS THAT MAY GUARD THE DOOR WHEN THE BOUND ONE IS GONE, in the order the benchmark supports (L11, 2026-09-30, `bench/judges`): a gate is chosen on its FALSE-ACCEPT rate, not its
/// price. Cursor's Grok-high caught every broken case of the first set (0/50 false accepts, 96 % right); Composer is the cheap second (0/50 on the first set, 20/75 = 27 % false accepts on the
/// harder one); Claude's own models follow; Gemini rubber-stamps the harder set and Codex rejects 4 good runs in 5, so they are last. A list in one place, to be re-ordered by the record.
/// Checked 2026-10-01 against the complete harder set (n = 75 broken a seat), and CORRECTED the same day (HANDOFF §6.1 "Order check"): false accepts Opus 14, Grok-high 18, Composer 20, Sonnet 26, Haiku 27, and
/// without the frontend class every seat fails alike, Opus 0/60, Grok 4/60, Composer 6/60, Sonnet 12/59. By the ONE rule on record (a gate is chosen on its false-accept rate) Opus would be first; the order
/// below is the operator's 2026-09-30 decision, made from the first set for a machine whose Claude is off, and it has NOT been re-ordered - whether Opus goes first when Claude is on is the operator's call.
/// (An earlier version of this comment called false-reject a tie-break "of the operator's rule": it is not on record.) Gemini flash (27/45) and Codex (no v2) have incomplete cells; Gemini default is 15/15 on backend and unread elsewhere.
pub const QA_GATE_ORDER: &[(Engine, &str)] = &[(Engine::Cursor, "cursor-grok-4.6-high"), (Engine::Cursor, "composer-2.5"), (Engine::ClaudeCode, "sonnet"), (Engine::Gemini, "gemini-3.8-flash-medium"), (Engine::Codex, "gpt-5.6-terra@medium")];   // Opus was on this roster; removed (operator 2026-11-13: never Opus)
/// THE QA GATE'S MEASURED TABLE (operator 2026-10-07: "Pick, route. Heimdall should route always. Deterministically."): per seat and defect class, `(false accepts, broken runs shown, false rejects, good runs shown)`
/// from the v2 lanes (bench/judges, `python3 bench/judges/l11v2-table.py`, 2026-10-07; every cell n = 13-15). The ONE input of `qa_gate_ranking`; re-measure, edit the numbers, and the order follows.
pub const QA_CLASSES: [&str; 5] = ["backend", "frontend", "design", "docs", "infra"];
pub const QA_GATE_TABLE: &[(Engine, &str, [(u8, u8, u8, u8); 5])] = &[
    (Engine::Cursor, "cursor-grok-4.6-high", [(4, 15, 2, 15), (7, 15, 0, 15), (0, 15, 5, 15), (0, 15, 0, 15), (0, 15, 0, 15)]),
    (Engine::Cursor, "composer-2.5", [(5, 15, 2, 15), (11, 15, 2, 15), (1, 15, 10, 15), (0, 15, 1, 15), (0, 15, 0, 15)]),
    (Engine::ClaudeCode, "sonnet", [(8, 15, 0, 15), (14, 15, 0, 15), (4, 15, 3, 15), (0, 14, 15, 15), (0, 15, 0, 15)]),
    (Engine::Gemini, "gemini-3.8-flash-medium", [(14, 15, 0, 15), (13, 15, 0, 15), (0, 15, 0, 15), (0, 15, 8, 15), (0, 15, 0, 15)]),
    (Engine::Codex, "gpt-5.6-terra@medium", [(0, 15, 14, 15), (0, 15, 13, 15), (0, 13, 14, 15), (0, 15, 15, 15), (0, 15, 1, 15)]),
];
/// A gate must TELL good from broken before its false-accept rate means anything: Youden's J = (1 - false-accept rate) - false-reject rate. Codex medium has 0 false accepts in 73 broken runs and rejects
/// 57 of 75 GOOD ones: it says "no" to nearly everything, so "never lets a broken run through" costs it nothing and tells us nothing (J = 0.24 pooled). Below this a seat is only a last resort.
pub const QA_INFORMATIVE: f64 = 0.25;
/// What a call to each gate seat costs, cheapest first - a tie-break only (measured per-call figures: Gemini flash $0.0036 and Codex terra $0.030 on a one-claim check, 2026-10-09; Grok-high about $0.25 and Opus
/// $0.14-0.28 per QA call in the recent runs; Composer rides Cursor's Auto pool and Sonnet is cheaper than Opus). Re-order it by the record, never to favour a seat the table does not.
pub const QA_GATE_PRICE: &[(Engine, &str)] = &[(Engine::Gemini, "gemini-3.8-flash-medium"), (Engine::Cursor, "composer-2.5"), (Engine::Codex, "gpt-5.6-terra@medium"), (Engine::ClaudeCode, "sonnet"), (Engine::ClaudeCode, "opus"), (Engine::Cursor, "cursor-grok-4.6-high")];
fn qa_price_rank(e: Engine, m: &str) -> usize { QA_GATE_PRICE.iter().position(|(pe, pm)| *pe == e && *pm == m).unwrap_or(QA_GATE_PRICE.len()) }
fn r6(x: f64) -> f64 { (x * 1e6).round() / 1e6 }
/// THE ORDER A GATE IS CHOSEN IN, for one defect class (`None` = every class pooled - an unknown or mixed change). Deterministic, from the table alone:
///   1. seats that are INFORMATIVE (J >= `QA_INFORMATIVE`) before the rest;
///   2. among them, the LOWEST FALSE-ACCEPT RATE first (the operator's rule: a gate is chosen on its false-accept rate, not its price), then the higher J, then the roster order (cheaper first);
///   3. the rest by J, then false accepts, then the roster order.
/// A TIE ON BOTH NUMBERS IS BROKEN BY PRICE, cheapest first (`QA_GATE_PRICE`), and only then by the roster (2026-10-10: on infra five seats are perfect, 0 false accepts and 0 false rejects of 15 and 15, and the roster put
/// Cursor's Grok-high first - about $0.25 a call against $0.004 for Gemini flash on a one-claim check, bench/qa-mutants - for the same measured result). The numbers decide first, always: a cheaper seat that is
/// even one false accept worse never moves up.
/// Pure - tested.
pub fn qa_gate_ranking(class: Option<&str>) -> Vec<(Engine, &'static str)> {
    let k = class.and_then(|c| QA_CLASSES.iter().position(|x| *x == c));
    let mut rows: Vec<(usize, Engine, &'static str, f64, f64)> = QA_GATE_TABLE.iter().enumerate().map(|(i, (e, m, cells))| {
        let (fa, nb, fr, ng) = match k {
            Some(k) => { let c = cells[k]; (c.0 as u32, c.1 as u32, c.2 as u32, c.3 as u32) }
            None => cells.iter().fold((0, 0, 0, 0), |a, c| (a.0 + c.0 as u32, a.1 + c.1 as u32, a.2 + c.2 as u32, a.3 + c.3 as u32)),
        };
        let (far, frr) = (fa as f64 / nb.max(1) as f64, fr as f64 / ng.max(1) as f64);
        (i, *e, *m, r6(far), r6(1.0 - far - frr))
    }).collect();
    rows.sort_by(|a, b| {
        let (ia, ib) = (a.4 >= QA_INFORMATIVE - 1e-9, b.4 >= QA_INFORMATIVE - 1e-9);
        ib.cmp(&ia).then_with(|| if ia { a.3.total_cmp(&b.3).then(b.4.total_cmp(&a.4)) } else { b.4.total_cmp(&a.4).then(a.3.total_cmp(&b.3)) })
            .then_with(|| qa_price_rank(a.1, a.2).cmp(&qa_price_rank(b.1, b.2))).then(a.0.cmp(&b.0))
    });
    rows.into_iter().map(|r| (r.1, r.2)).collect()
}
/// The defect class a change belongs to, from `roles::Domain::key()` - `None` (pooled) for tests, mixed, unknown.
pub fn qa_class(domain_key: &str) -> Option<&'static str> { QA_CLASSES.iter().copied().find(|c| *c == domain_key) }
/// The first seat of the class's ranking that can take a call today. Pure over the availability question.
pub fn qa_gate_pick(class: Option<&str>, ok: impl Fn(Engine, Option<&str>) -> bool) -> Option<Binding> {
    qa_gate_ranking(class).into_iter().find(|(e, m)| ok(*e, Some(m))).map(|(e, m)| Binding { engine: e, model: Some(m.to_string()), source: "qa-gate-ranking" })
}
/// May this seat take a call today: switched on, signed in, and not out of quota or nearly out of a window.
pub fn seat_ok(engine: Engine, model: Option<&str>) -> bool { engine_routable(engine) && model_blocked(engine, model).is_none() }
/// A binding whose seat cannot take a call (switched off in Settings, out of quota, signed out) is replaced BEFORE anything starts (operator 2026-09-30: his Claude subscription ends today
/// and the roles file binds the planner and the QA to Claude). The QA goes down `QA_GATE_ORDER`; the others go to the seat Heimdall itself would use. Pure over the availability question.
pub fn binding_when_unavailable(role: Role, b: Binding, run_engine: Engine, ok: impl Fn(Engine, Option<&str>) -> bool, heimdall: impl Fn() -> Option<Engine>) -> Binding {
    if ok(b.engine, b.model.as_deref()) { return b }
    if role == Role::Qa { if let Some(b) = qa_gate_pick(None, &ok) { return Binding { source: "unavailable-seat", ..b } } }
    let e = heimdall().unwrap_or(run_engine);
    Binding { engine: e, model: tier_model(e, role.legacy_tier()), source: "unavailable-seat" }
}
/// A CALL THAT NAMES A SEAT IS NOT BOUND TO IT WHEN IT IS GONE (2026-10-01, the first real run with Claude switched off: every X-ray feature, the episode namer and the raw comparison asked for
/// `cli:claude-code` by default - the FE pins it, a pane's last engine carries it - and each came back "Claude is switched off" instead of an answer). The seat asked for stands while it can take a
/// call; otherwise the seat Heimdall itself would plan on (`heimdall_engine`). `None` only when nothing at all can run, and the caller then keeps its own seat so the refusal it gets is the honest one.
/// Pure over the two questions - tested.
pub fn seat_or_heimdall_with(engine: Engine, ok: impl Fn(Engine) -> bool, heimdall: impl Fn() -> Option<Engine>) -> Option<Engine> {
    if ok(engine) { Some(engine) } else { heimdall() }
}
pub fn seat_or_heimdall(engine: Engine) -> Engine {
    seat_or_heimdall_with(engine, |e| seat_ok(e, None), || heimdall_engine(Some(engine))).unwrap_or(engine)
}
/// The binding for a role, given the run's own engine (what the pane is on) — the operator's file first, then the run. A seat that cannot take a call today is never returned.
pub fn role_binding(role: Role, run_engine: Engine) -> Binding { role_binding_in(role, run_engine, None) }
/// `role_binding` for a change of a known defect class: an UNPINNED QA gate is routed by Heimdall from `qa_gate_ranking(class)` - always, Claude on or off (operator 2026-10-07); the operator's PIN still wins.
pub fn role_binding_in(role: Role, run_engine: Engine, class: Option<&str>) -> Binding {
    if role == Role::Qa && role_pin(role).is_none() { if let Some(b) = qa_gate_pick(class, seat_ok) { return b } }
    binding_when_unavailable(role, role_binding_raw(role, run_engine), run_engine, seat_ok, || heimdall_engine(Some(run_engine)))
}
/// THE WORD FOR "THE ROUTER DECIDES" (operator 2026-10-01, "Engine config also"): a role is either `auto` - the default, the router picks per job from the benchmarks - or PINNED by the
/// operator to a seat and/or a model. `roles.<role> = {"engine": "auto"}` says it in the file; an absent entry means the same.
pub const ROLE_AUTO: &str = "auto";
/// What the operator pinned a role to - `None` when it is auto (absent, `engine: "auto"`, or empty). Pure over the file.
pub fn role_pin(role: Role) -> Option<(Option<Engine>, Option<String>)> {
    let all = chosen_models();
    let r = all.get("roles").and_then(|r| r.get(role.key()))?;
    let name = r.get("engine").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if name.eq_ignore_ascii_case(ROLE_AUTO) { return None }
    let engine = engine_of(&json!(name));
    let model = r.get("model").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()).map(String::from);
    if engine.is_some() || model.is_some() { Some((engine, model)) } else { None }
}
fn role_binding_raw(role: Role, run_engine: Engine) -> Binding {
    if let Some((engine, model)) = role_pin(role) {
        let engine = engine.unwrap_or(run_engine);
        return Binding { engine, model: model.or_else(|| tier_model(engine, role.legacy_tier())), source: "role" };
    }
    // no role binding: the JOB the role does decides (2026-09-22) — the measured winner, the operator's cell if set
    let job = match role { Role::Planner => Job::Planner, Role::Worker => Job::EditorL, Role::Qa => Job::QaGate };
    job_binding(job, run_engine, None).0
}
/// THE MODELS BAR (PLAN-THE-DIAL §2 — operator 2026-09-22: "cheaper balanced and faster"). The bar does not name
/// models: it walks the role's binding one step DOWN (0 = CHEAPER) or one step UP (2 = SMARTER) the LADDER THE
/// OPERATOR THEMSELF CONFIGURED for that engine on the Costs page. 1 (BALANCED) is their binding, untouched.
/// Pure over `chosen_models()` — tested.
pub fn ladder(engine: Engine) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for t in [Tier::Low, Tier::Mid, Tier::High] {
        if let Some(m) = tier_model(engine, t) { if !out.contains(&m) { out.push(m); } }
    }
    out
}
/// `model` moved `step` rungs on that engine's ladder, clamped at both ends. A model that is not on the ladder
/// (the operator typed one by hand) is anchored at `anchor` — the tier the role would have used.
pub fn shift_model(engine: Engine, model: Option<&str>, anchor: Tier, step: i8) -> Option<String> {
    let l = ladder(engine);
    if l.is_empty() { return model.map(String::from); }
    let at = model.and_then(|m| l.iter().position(|x| x == m))
        .unwrap_or(match anchor { Tier::Low => 0, Tier::Mid => 1.min(l.len() - 1), Tier::High => l.len() - 1 });
    let next = (at as i64 + step as i64).clamp(0, l.len() as i64 - 1) as usize;
    Some(l[next].clone())
}
/// The role's binding as the dial's models bar leaves it. `cost`: 0 CHEAPER · 1 BALANCED · 2 SMARTER; None = 1.
pub fn role_binding_at(role: Role, run_engine: Engine, cost: Option<u8>) -> Binding { role_binding_at_in(role, run_engine, cost, None) }
pub fn role_binding_at_in(role: Role, run_engine: Engine, cost: Option<u8>, class: Option<&str>) -> Binding {
    let b = role_binding_in(role, run_engine, class);
    let step: i8 = match cost { Some(0) => -1, Some(2) => 1, _ => 0 };
    if step == 0 || b.source == "qa-gate-ranking" { return b; }   // a gate is the seat the benchmark measured: the dial's bar does not move it to a model nobody measured as a gate
    let model = shift_model(b.engine, b.model.as_deref(), role.legacy_tier(), step);
    Binding { engine: b.engine, model, source: if b.source == "role" { "role+bar" } else { "tier+bar" } }
}
/// WHAT A MODEL COSTS, ACROSS FAMILIES — dollars per 1M output tokens, the one number the "a worker is always
/// cheaper than its planner" rule can compare between a Claude alias, a Codex effort and an OpenCode catalogue
/// entry. Aliases first (a model is named, not tiered), then the engine's own mid price for anything unknown. Pure.
/// ONE PRICE TABLE, NOT TWO (2026-09-27). This function used to carry its own copy of `prices_of`'s if-chain and had
/// drifted from it: qwen priced at $2.19 here and $1.20 there, and neither Composer nor Grok nor Gemini appeared at
/// all — so `composer-2.5` fell through to Cursor's mid rung and priced at **$15.00 instead of $1.50**. That is not a
/// cosmetic error: `worker_under_planner` compares exactly this number, so a Sonnet brain (15.00) over Composer hands
/// (15.00) is "not cheaper", and the router would have moved every Composer worker off Cursor. The output price IS
/// the ceiling the pair rule uses, so it is read from the one table.
pub fn price_of(engine: Engine, model: Option<&str>) -> f64 { prices_of(engine, model).1 }
/// THE PRICE LIST, PER MODEL — dollars per 1M tokens (input, output), the deterministic calculator every audit row
/// is priced with (operator 2026-09-23: "cost — a deterministic calculator based on model vs tokens"). Cache reads
/// are billed at a tenth of input, cache writes at 1.25× (Anthropic's terms); OpenCode's Chinese models at their
/// catalogue prices; Codex's efforts on the account's one model. An unknown name prices as its engine's mid tier.
pub fn prices_of(engine: Engine, model: Option<&str>) -> (f64, f64) {
    let m = model.unwrap_or("").to_ascii_lowercase();
    if m.contains("opus") { return (5.0, 25.0); }
    if m.contains("sonnet") { return (3.0, 15.0); }
    if m.contains("haiku") { return (1.0, 5.0); }
    if m.contains("deepseek") && m.contains("flash") { return (0.14, 0.28); }
    if m.contains("deepseek") { return (0.55, 2.19); }
    if m.contains("qwen") { return (0.40, 1.20); }
    if m.contains("kimi") { return (0.60, 2.50); }
    if m.contains("glm") { return (0.60, 2.20); }
    if m.contains("minimax") { return (0.30, 1.20); }
    // GEMINI FLASH IS PRICED LIKE THE CHEAPEST CHINESE MODEL (2026-09-27) — $0.14/$0.28, identical to
    // deepseek-v4-flash. That is the finding that moved this product off the Chinese models: they were never cheap
    // against Gemini, only against Sonnet. Caught by the `flash`/`lite` rule below, which comes first.
    if m.contains("grok") && (m.contains("fast") || m.contains("code") || m.contains("mini")) { return (0.20, 1.50); }
    if m.contains("grok") { return (3.0, 15.0); }
    if m.contains("gemini") && m.contains("pro") { return (1.25, 10.0); }
    if m.contains("gemini") && (m.contains("flash") || m.contains("lite")) { return (0.14, 0.28); }
    // Composer 2.5 is $0.50/M input (Cursor's published figure) and carries no token rate against the plan's own-model
    // pool; the output figure is an estimate, as Cursor does not publish one.
    if m.contains("composer") { return (0.50, 1.50); }
    // "GEMINI" CONTAINS "MINI" (audit 2026-09-28). With the cheap-family test first, every Gemini id that is not
    // caught by the `pro` guard above — `gemini-3.1-pro` aside, that is the account default and anything new —
    // matched `mini` and was priced at $0.14/$0.28, the cheapest rate in this table, and the line below was dead
    // code. It flattered Gemini in every merge comparison and every saving measured against it. The family test
    // now excludes the word it is a substring of.
    if m.contains("gemini") { return (1.25, 10.0); }
    if m.contains("flash") || m.contains("mini") || m.contains("nano") || m.contains("lite") { return (0.14, 0.28); }
    if m.contains("gpt") || engine == Engine::Codex { return (1.25, 10.0); }
    let (_, i, o) = list_price(engine, Tier::Mid); (i, o)
}
/// The deterministic cost of one call. Pure — tested.
pub fn price_tokens(engine: Engine, model: Option<&str>, tokens_in: i64, tokens_out: i64, cache_read: i64, cache_write: i64) -> f64 {
    let (pin, pout) = prices_of(engine, model);
    (tokens_in.max(0) as f64 * pin + cache_read.max(0) as f64 * pin * 0.1 + cache_write.max(0) as f64 * pin * 1.25 + tokens_out.max(0) as f64 * pout) / 1e6
}
/// THE FRONTIER YARDSTICK (operator 2026-09-28: "the comparison uses a deterministic calculator that compares with
/// official prices of tokens from the best frontier model, for claude or codex — the most used model of those
/// providers: Claude Opus and the Codex equivalent"). Every "saved" figure is the SAME tokens priced at this model's
/// list rate against what the work really cost — never against the seat's own top rung, which for four of the six
/// engines is the same rate as the cheap one and made the product announce "saved $0.00" on work that was 5× cheaper
/// than Opus. A Codex seat is measured against Codex's frontier model; every other seat against Claude Opus.
/// Returns the engine and model to price with, and the label the screen prints. Pure — tested.
pub fn frontier_of(engine: Engine) -> (Engine, &'static str, &'static str) {
    match engine {
        Engine::Codex => (Engine::Codex, "gpt-5", "codex · gpt-5"),
        _ => (Engine::ClaudeCode, "opus", "claude · opus"),
    }
}
/// The frontier model's price for these tokens (cache reads and writes weighted like `price_tokens`), and its label.
pub fn frontier_price(engine: Engine, tokens_in: i64, tokens_out: i64, cache_read: i64, cache_write: i64) -> (f64, &'static str) {
    let (fe, fm, label) = frontier_of(engine);
    (price_tokens(fe, Some(fm), tokens_in, tokens_out, cache_read, cache_write), label)
}
/// …EXCEPT WHEN NO PLANNER WAS CALLED (item 4, 2026-10-01). "Always one worker cheaper than its planner" is the rule of a merge —
/// a dearer brain decides, a cheaper hand writes. A run at L or smaller calls no planner at all (`plan_basis == "solo"`): the one
/// seat the record chose among the solo cells and the pairs, by the one tolerance, IS the answer, and holding it under the price of
/// a planner that was never called would swap away the very seat that won on value. A seat that cannot take a call still goes
/// through `worker_under_planner`, which is also what moves it off. Pure over the availability question — tested.
pub fn worker_for_run(asked: Binding, planner: &Binding, solo: bool, usable: impl Fn(Engine) -> bool) -> (Binding, Option<String>) {
    if solo && usable(asked.engine) { return (asked, None) }
    worker_under_planner(asked, planner)
}
/// THE WORKER IS ALWAYS CHEAPER THAN ITS PLANNER (operator 2026-09-22: "Heimdall routes not just between models on
/// the same family, but by different — always one worker cheaper than its planner"). The worker walks DOWN its own
/// ladder first; if its engine has nothing cheaper left, it crosses to the cheapest engine whose bottom rung is
/// under the planner. Returns the binding it settled on and, when it had to move, the line to log. Pure — tested.
pub fn worker_under_planner(worker: Binding, planner: &Binding) -> (Binding, Option<String>) {
    let ceiling = price_of(planner.engine, planner.model.as_deref());
    let asked = (worker.engine, worker.model.clone());
    // A DISALLOWED ENGINE IS NOT MADE ALLOWED BY BEING CHEAP (2026-09-27, caught by
    // `the_cross_family_fallback_never_lands_on_a_provider_that_is_off`). Both shortcuts below stay on the engine
    // they were handed: the early return keeps it if it is already under the ceiling, and the ladder walk keeps it
    // whenever a cheaper model exists on it. So a worker pinned to OpenCode stayed on OpenCode with Zen switched
    // off — the provider was "off" everywhere except in the one function that assigns the work.
    let reachable = engine_usable(worker.engine);
    if reachable && price_of(worker.engine, worker.model.as_deref()) < ceiling { return (worker, None); }
    let mut b = worker;
    // down its own ladder — only if we are allowed to stay on this engine at all
    if reachable {
        let l = ladder(b.engine);
        if let Some(cheapest) = l.first() {
            if price_of(b.engine, Some(cheapest)) < ceiling {
                for m in l.iter() { if price_of(b.engine, Some(m)) < ceiling { b.model = Some(m.clone()); break; } }
            }
        }
    }
    // across families, cheapest first — only engines that can actually be reached (a ladder means a model to pass)
    if !reachable || price_of(b.engine, b.model.as_deref()) >= ceiling {
        let mut best: Option<(f64, Engine, String)> = None;
        // ONLY ENGINES THE ROUTER IS ALLOWED TO REACH (2026-09-27). This loop used to name three engines
        // unconditionally, so a worker could be pushed onto OpenCode — a metered gateway that is off by default —
        // purely because it was the cheapest rung. That is how a "deterministically off" provider leaks back in.
        for e in Engine::all() {
            if !engine_usable(e) { continue; }
            if let Some(m) = ladder(e).into_iter().next() {
                let p = price_of(e, Some(&m));
                if p < ceiling && best.as_ref().is_none_or(|(bp, ..)| p < *bp) { best = Some((p, e, m)); }
            }
        }
        if let Some((_, e, m)) = best { b.engine = e; b.model = Some(m); }
    }
    if (b.engine, b.model.clone()) == asked { return (b, None); }
    let note = format!("the worker must be cheaper than its planner ({} {} · ${:.2}/1M out): {} {} → {} {}",
        planner.engine.key(), planner.model.as_deref().unwrap_or("default"), ceiling,
        asked.0.key(), asked.1.as_deref().unwrap_or("default"), b.engine.key(), b.model.as_deref().unwrap_or("default"));
    (Binding { source: "role+cheaper", ..b }, Some(note))
}
/// THE JOBS (operator 2026-09-22: "each worker can have a list of jobs … winner + fallback always"). What a run
/// asks a model to do is one of these ten, and the router picks BY JOB, deterministically, from the table below —
/// measured on 2026-09-22 (bench/ladder/ONE-WORKER-RESULTS.md) and overridable per cell in engine-models.json.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Job { Lookup, Synthesis, Screening, Planner, EditorS, EditorL, Verifier, TestWriter, QaGate, Report }
impl Job {
    pub const ALL: [Job; 10] = [Job::Lookup, Job::Synthesis, Job::Screening, Job::Planner, Job::EditorS, Job::EditorL, Job::Verifier, Job::TestWriter, Job::QaGate, Job::Report];
    pub fn key(self) -> &'static str { match self { Job::Lookup => "lookup", Job::Synthesis => "synthesis", Job::Screening => "screening", Job::Planner => "planner", Job::EditorS => "editor-s", Job::EditorL => "editor-l", Job::Verifier => "verifier", Job::TestWriter => "test-writer", Job::QaGate => "qa-gate", Job::Report => "report" } }
    pub fn of(s: &str) -> Option<Job> { Job::ALL.into_iter().find(|j| j.key() == s.trim().to_ascii_lowercase()) }
    /// one line the settings screen shows beside the job
    pub fn what(self) -> &'static str { match self {
        Job::Lookup => "where / which / who — a question answered by finding the place",
        Job::Synthesis => "why / how / in what order — a question answered by explaining",
        Job::Screening => "is this ask assumable or blocking — the question round before a plan",
        Job::Planner => "the goal → the contract: modules, signatures, acceptance",
        Job::EditorS => "one file, one short ask — written raw, checked by the compiler",
        Job::EditorL => "a module or several — written under a verifier",
        Job::Verifier => "one task's diff against its acceptance: pass or send back",
        Job::TestWriter => "one test case per acceptance claim",
        Job::QaGate => "the optional gate: one isolated judgment per claim",
        Job::Report => "the consolidated report, episode titles, summaries" } }
}
/// THE MEASURED WINNERS AND FALLBACKS, per engine. Claude Code and OpenCode are the benches of 2026-09-22; Codex is
/// BY ANALOGY until the Oct 9 window measures it (the account exposes one model and a reasoning effort, so its cells
/// are efforts: low ≈ Haiku/Flash, medium ≈ Sonnet/Pro, high ≈ Opus). Pure.
pub fn job_defaults(job: Job, engine: Engine) -> (&'static str, &'static str) {
    use Engine::*; use Job::*;
    // GROK IS SHAPED LIKE CODEX (2026-09-27): one account model at a reasoning effort, so its per-job cells ARE
    // Codex's cells — stated by reuse rather than copied, so the two can never drift apart.
    if engine == Grok { return job_defaults(job, Codex); }
    // GEMINI AND CURSOR HAVE NO MEASURED CELLS YET. "" means "the job decides" (see `model_name`), which is the only
    // honest value for a seat that has never answered a call here — a guessed model id would be a runtime error on
    // every request. They fill in from bench/BENCH.md once the seats are signed in.
    if matches!(engine, Gemini | Cursor) { return ("", ""); }
    match (engine, job) {
        (ClaudeCode, Lookup) => ("haiku", "sonnet"),          (Opencode, Lookup) => ("opencode/deepseek-v4-pro", "opencode/deepseek-v4-flash"),   (Codex, Lookup) => ("low", "medium"),
        (ClaudeCode, Synthesis) => ("sonnet", "opus"),        (Opencode, Synthesis) => ("opencode/qwen3.6-plus", "opencode/deepseek-v4-pro"), (Codex, Synthesis) => ("medium", "high"),
        (ClaudeCode, Screening) => ("haiku", "sonnet"),       (Opencode, Screening) => ("opencode/deepseek-v4-flash", "opencode/deepseek-v4-pro"), (Codex, Screening) => ("low", "medium"),
        (ClaudeCode, Planner) => ("sonnet", "opus"),          (Opencode, Planner) => ("opencode/deepseek-v4-pro", "opencode/deepseek-v4-pro"),   (Codex, Planner) => ("medium", "high"),
        (ClaudeCode, EditorS) => ("sonnet", "opus"),          (Opencode, EditorS) => ("opencode/qwen3.6-plus", "opencode/deepseek-v4-pro"), (Codex, EditorS) => ("medium", "high"),
        (ClaudeCode, EditorL) => ("haiku", "sonnet"),         (Opencode, EditorL) => ("opencode/deepseek-v4-flash", "opencode/deepseek-v4-pro"), (Codex, EditorL) => ("low", "medium"),
        (ClaudeCode, Verifier) => ("sonnet", "opus"),         (Opencode, Verifier) => ("opencode/deepseek-v4-pro", "opencode/deepseek-v4-pro"),  (Codex, Verifier) => ("medium", "high"),
        (ClaudeCode, TestWriter) => ("sonnet", "opus"),       (Opencode, TestWriter) => ("opencode/deepseek-v4-pro", "opencode/deepseek-v4-pro"), (Codex, TestWriter) => ("medium", "high"),
        // THE GATE IS SONNET — AND IT IS NOT CHOSEN ON PRICE (settled 2026-09-27 after getting it wrong twice).
        //
        // THE TOLERANCE RULE DOES NOT APPLY HERE, and that is the whole point. "Within 10 quality points and at least
        // 2x cheaper wins" is sound wherever a failure is RECOVERABLE: a writer that slips is caught by the gate, so a
        // few points of quality are worth a large price cut. NOTHING CATCHES THE GATE. It is the last line, so its loss
        // function is asymmetric — a false accept ships bad code at unbounded cost, against a saving of fractions of a
        // cent on one call per run. Applying the price rule to the gatekeeper was a category error.
        //
        // The measurement already said so and was under-weighted. Over both graded fixtures (bench/qa-gate's 4 planted
        // defects beside 4 correct runs, and bench/qa-subtle's 2 real defects that compile cleanly), sonnet scored
        // 12/12 with ZERO false accepts and ZERO false rejects. Haiku scored 11/12 and its single failure was a FALSE
        // ACCEPT: it believed a report claiming unit tests that exist nowhere in the diff or the worktree. That is
        // precisely the gatekeeper failure, and it is not interchangeable with a writer's slip.
        //
        // Opus is not the answer either, on a hard fact rather than a price: it could not produce a verdict at all,
        // 6 of 12 cases silent even at a 30-turn cap. gemini-3.8-flash and cursor-grok also scored 12/12 and are the
        // arms to prefer when the run is already on those seats; `job_global` reaches them from the record.
        //
        // WHAT THE FIXTURES DO NOT YET TEST (operator 2026-09-27: "QA should make a code review on the deliveried diff
        // … based on the criteria, what was asked, what was deliveried and the context of the code and x-ray intel").
        // Twelve checks of PLANTED defects are not a code review. A real gate faces an arbitrary diff against arbitrary
        // criteria, and `goal.qa_review` is handed only {{goal}}, {{plan}}, {{report}}, {{diff}} — it may open files,
        // but it receives NO x-ray intelligence at all. Until a fixture exercises that, no cheap arm has earned this
        // seat, and the burden of proof sits with the cheaper model rather than with the careful one.
        (ClaudeCode, QaGate) => ("sonnet", "opus"),          (Opencode, QaGate) => ("opencode/deepseek-v4-pro", "opencode/deepseek-v4-pro"),    (Codex, QaGate) => ("high", "medium"),
        (ClaudeCode, Report) => ("haiku", "sonnet"),          (Opencode, Report) => ("opencode/deepseek-v4-flash", "opencode/deepseek-v4-pro"),  (Codex, Report) => ("low", "medium"),
        // handled by the two early returns above; named rather than caught by `_` so that a SEVENTH engine is a
        // compile error here and not a silent ("", "")
        (Grok, _) | (Gemini, _) | (Cursor, _) => ("", ""),
    }
}
/// The winner and the fallback for a job on an engine — the operator's cell in engine-models.json (`jobs.<job>.<engine>
/// = {winner, fallback}`) over the measured default — each moved by the models bar like a role is. Pure over the file.
pub fn job_binding(job: Job, engine: Engine, cost: Option<u8>) -> (Binding, Binding) {
    let (dw, df) = job_defaults(job, engine);
    let cell = chosen_models().get("jobs").and_then(|j| j.get(job.key())).and_then(|j| j.get(engine.key())).cloned().unwrap_or(Value::Null);
    let pick = |k: &str, d: &str| -> (String, &'static str) { match cell.get(k).and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()) { Some(m) => (m.to_string(), "job"), None => (d.to_string(), "measured") } };
    let (w, ws) = pick("winner", dw); let (f, fs) = pick("fallback", df);
    let step: i8 = match cost { Some(0) => -1, Some(2) => 1, _ => 0 };
    let shift = |m: String| if step == 0 || engine == Engine::Codex { m } else { shift_model(engine, Some(&m), Tier::Mid, step).unwrap_or(m) };
    (Binding { engine, model: named_model(shift(w)), source: ws }, Binding { engine, model: named_model(shift(f)), source: fs })
}
/// "" MEANS "THE TIER DECIDES", NEVER A MODEL NAME (found by the Claude-off journey, 2026-10-05). Gemini and Cursor have no measured job cells, so their defaults are ("", ""); a `Some("")` binding
/// reached `model_blocked`, which refused it as "not one of Cursor's own models" - on any machine whose merge record had no Cursor row yet (a fresh install on one seat), every worker died on it.
/// `None` lets `run_cli_inner` resolve the engine's tier model instead.
fn named_model(m: String) -> Option<String> { if m.trim().is_empty() { None } else { Some(m) } }
/// THE GLOBAL WINNER (operator 2026-09-22: "the winner must be DeepSeek v4 Pro — even though it has 1 less than
/// Sonnet it is 10× cheaper; for a difference of 1 to 3 we still choose the cheaper as winner, in any job"). Across
/// providers, the cheapest model keeping at least 80 % of the best measured quality is the winner and the next is the fallback,
/// on another provider where one measured. A job with no cross-provider measurement keeps the measured model.
///   lookup      Pro 28/30 · Haiku 28/30           → Pro, Haiku      synthesis  Sonnet 17/20 · Pro 13/20 (4 apart) → Sonnet, Pro
///   editor-s    Pro 4/5 · Sonnet 5/5              → Pro, Sonnet     editor-l   Flash 35–37 · Pro 36–37            → Flash, Pro
///   planner     Pro 37/37 · Sonnet 35–36          → Pro, Sonnet     verifier   Sonnet measured, Pro not           → Sonnet, Pro
///   test-writer Sonnet measured, Pro not          → Sonnet, Pro     qa-gate    Opus found what Pro's judge missed → Opus, Pro
///   report / screening: output-only, cheapest      → Flash, Haiku
pub fn job_global(job: Job) -> ((Engine, &'static str), (Engine, &'static str)) {
    use Engine::*; use Job::*;
    match job {
        Lookup => ((Opencode, "opencode/deepseek-v4-pro"), (ClaudeCode, "haiku")),
        // reader, 2026-09-23, same blind pass: synthesis Sonnet 16/20 · Qwen 14/20 · Pro 13/20. Two of twenty is five
        // of fifty - a 12 % gap in relative terms, which the 80 % band (2026-09-30) would now let price decide; the measured rows decide it - so Sonnet keeps synthesis; Qwen (cheapest, 7× under Sonnet) is
        // the fallback. Qwen's 14 was one 10/10 and one 4/8 that called the commit phase unable to throw.
        Synthesis => ((ClaudeCode, "sonnet"), (Opencode, "opencode/qwen3.6-plus")),
        Screening => ((Opencode, "opencode/deepseek-v4-flash"), (ClaudeCode, "haiku")),
        Planner => ((Opencode, "opencode/deepseek-v4-pro"), (ClaudeCode, "sonnet")),
        // S ×5 raw, 2026-09-23: Qwen 3.6 Plus 5/5 · 34 s · $0.011 — as right as Sonnet (5/5, $0.093) and Opus, 8× cheaper;
        // Pro was 4/5. Inside the band and cheaper → Qwen wins editor-s, Sonnet stays the fallback.
        EditorS => ((Opencode, "opencode/qwen3.6-plus"), (ClaudeCode, "sonnet")),
        // L, 2026-09-23: as the hands under a brain, Flash scored 36-37/37 over three runs at $0.14/M — GLM, Qwen and
        // Pro each hit 37/37 once, all inside the quality band and all dearer, so the cheapest keeps the job.
        EditorL => ((Opencode, "opencode/deepseek-v4-flash"), (Opencode, "opencode/deepseek-v4-pro")),
        // THE VERIFIER, 2026-09-23. The graded fixtures (bench/jobs) do NOT discriminate — nine models, all 5/5 — so
        // they cannot be used to pick a winner on quality; what decides it is the table's own audited calls, where
        // Qwen has ten of them at 100 % against Sonnet's five, at a seventh of Sonnet's price. Sonnet is the fallback.
        Verifier => ((Opencode, "opencode/qwen3.6-plus"), (ClaudeCode, "sonnet")),
        TestWriter => ((ClaudeCode, "sonnet"), (Opencode, "opencode/deepseek-v4-pro")),
        // THE GATE, 2026-09-23 — the one job where the graded fixtures DO discriminate, and they invert the price
        // order: Haiku, Qwen, Codex terra and Codex sol answer 4/4, Claude Sonnet 2/4 (it says `null` on a claim
        // plainly met and `false` on one that cannot be settled), and Codex at HIGH reasoning effort does worse than
        // at medium. Opus was the default and has no graded score at all, at $0.124 a check. Four models tie on
        // correctness, so the tolerance rule hands it to the cheapest: Qwen, with Haiku behind it.
        QaGate => ((Opencode, "opencode/qwen3.6-plus"), (ClaudeCode, "haiku")),
        Report => ((Opencode, "opencode/deepseek-v4-flash"), (ClaudeCode, "haiku")),
    }
}
/// Is this engine's CLI on this machine? `which` once per process per engine. A test never shells out.
/// A SEAT, NOT A BINARY (operator 2026-09-26: "on antiloki only if i have a subscription").
///
/// An installed CLI is not a subscription. On the machine this was written, `gemini`, `grok` and `cursor-agent`
/// were all present and NONE of them was signed in — so a binary check would have offered the operator three
/// providers antiloki could not have made a single call to, and onboarding would have told someone with a Cursor
/// Pro plan that their subscription worked when nothing had been authenticated.
///
/// `Some(true)`  — a credential for this provider is on disk (or its key is in the environment).
/// `Some(false)` — we know where this provider keeps its credential and there is none.
/// `None`        — we do not know how to check this provider; the caller must not treat that as "no".
///
/// Deliberately conservative: a provider we cannot check is never DENIED on that basis, because a wrong `false`
/// hides a subscription the user is paying for. Only a provider we know how to check can be ruled out.
#[cfg(test)]
thread_local! { /** a test that exercises the real credential probe turns it on for its own thread */ pub static REAL_SEAT_CHECK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
/// A CREDENTIAL FILE THAT IS NOT THERE: "no" on Linux, "unknown" on Windows (2026-10-11). The paths above were found by looking on Linux; Cursor, Antigravity and the xAI CLI keep a Windows login in
/// places this probe has not seen (AppData, Credential Manager), so a missing file there proves nothing, and a wrong `false` fails closed: it hides a subscription the user pays for. `None` is
/// "maybe" (`engine_usable` accepts it, the onboarding's real test call then decides), and a file that IS there is a yes everywhere. Pure - tested.
pub fn seat_unsure(found: bool, windows: bool) -> Option<bool> { if found { Some(true) } else if windows { None } else { Some(false) } }
pub fn engine_seat(engine: Engine) -> Option<bool> {
    // UNIT TESTS DO NOT DEPEND ON THE MACHINE'S LOGINS (the first CI run, 2026-10-06: 13 serve tests failed on a runner with no Claude, Codex or Cursor sign-in because they read ~/.claude.json and friends). Under `cfg(test)` every seat
    // reads as signed in unless the test asks for the real probe.
    #[cfg(test)]
    { if !REAL_SEAT_CHECK.with(|c| c.get()) { return Some(true); } }
    let home = std::path::PathBuf::from(std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into()));
    let any = |rel: &[&str]| rel.iter().any(|r| home.join(r).exists());
    let env = |names: &[&str]| names.iter().any(|n| std::env::var(n).map(|v| !v.trim().is_empty()).unwrap_or(false));
    match engine {
        Engine::ClaudeCode => Some(any(&[".claude/.credentials.json", ".claude.json"]) || env(&["ANTHROPIC_API_KEY"])),
        Engine::Codex => Some(any(&[".codex/auth.json"]) || env(&["OPENAI_API_KEY"])),
        Engine::Opencode => Some(any(&[".local/share/opencode/auth.json", ".config/opencode/auth.json"])),
        // A CREDENTIAL, NOT A CONFIG FILE (2026-09-27). `~/.gemini/settings.json` exists on a machine that has never
        // signed in — the CLI's own error is "Please set an Auth method in your settings.json" — so testing for it
        // reports a seat that is not there. Only the OAuth artefacts and the env keys prove a seat.
        // ANTIGRAVITY KEEPS ITS CREDENTIALS IN THE OS KEYRING, NOT IN A FILE (its install/auth guide, read
        // 2026-09-27: Secret Service on Linux, Keychain on macOS, Credential Manager on Windows). So there is no
        // artefact on disk to test, and the file-based seat check this function performs for every other engine
        // CANNOT answer the question here. Reading the keyring would mean a dbus round-trip — and a possible unlock
        // prompt — on a call that every router decision makes, which is not acceptable for a cheap predicate.
        //
        // The heuristic is therefore the settings directory `agy` writes on first run, plus the legacy CLI's OAuth
        // artefacts and the env keys. It is deliberately the WEAKEST check in this function, and that is survivable
        // for one specific reason: per Antigravity's headless docs an unauthenticated run "exits with an
        // authentication required error instead of hanging", so a false positive here costs one fast, self-explaining
        // failure rather than a stalled run. A false NEGATIVE would be worse — it would hide a seat the operator has
        // paid for — which is why the directory counts.
        // A SUBSCRIPTION, NOT AN API KEY (operator 2026-09-27: "we only use subscriptions … gemini as well").
        // `GEMINI_API_KEY` / `GOOGLE_API_KEY` bill per token and `GOOGLE_GENAI_USE_VERTEXAI` bills a GCP project —
        // metered paths, so none of them constitutes a seat here even though the CLI would accept them. What counts is
        // Antigravity's own login (Google AI Pro/Ultra) or Code Assist (`GOOGLE_GENAI_USE_GCA`), which are plans.
        Engine::Gemini => seat_unsure(any(&[".gemini/antigravity-cli", ".config/antigravity",
                                     ".gemini/oauth_creds.json", ".gemini/google_accounts.json"])
            || env(&["GOOGLE_GENAI_USE_GCA"]), cfg!(windows)),
        Engine::Grok => seat_unsure(any(&[".grok/auth.json", ".grok/credentials.json", ".config/grok/auth.json"])
            || env(&["XAI_API_KEY", "GROK_API_KEY"]), cfg!(windows)),
        // THE REAL PATHS, FOUND BY LOOKING (2026-09-27). This guessed `.cursor/cli-config.json` and
        // `.config/cursor-agent/auth.json`; neither exists. `cursor-agent` signs in to `~/.config/cursor/auth.json`
        // and writes `~/.cursor/agent-cli-state.json` on first run — so a working, paid Cursor seat reported
        // `signed_in: false` and the router refused every Cursor arm while the CLI answered perfectly from the shell.
        // A guessed credential path is worse than none: it fails CLOSED and silently withholds a seat the operator pays for.
        // ONLY `auth.json` — it holds accessToken/refreshToken. `cli-config.json` (permissions, editor, hints) and
        // `agent-cli-state.json` (version, tips shown) are CONFIG and STATE: both exist without a login, and counting
        // them would report a seat that is not there — the same mistake `~/.gemini/settings.json` invites above.
        Engine::Cursor => seat_unsure(any(&[".config/cursor/auth.json"]) || env(&["CURSOR_API_KEY"]), cfg!(windows)),
    }
}

/// THE TWO PROVIDER SWITCHES (operator 2026-09-27: "add on settings an option to make Zen reachable or not … and by
/// default no reachable" · "a button to activate or deactivate Chinese models. Don't make Chinese models obligatory").
///
/// BOTH DEFAULT OFF, and that is what makes the 402 structurally impossible rather than merely unlikely. They are two
/// switches and not one because they answer different questions: Zen is a BILLING PATH (metered credit that can
/// empty), Chinese models are a MODEL FAMILY (which may later be reached through a direct key or another gateway).
/// With Zen on and Chinese off, OpenCode still serves its Gemini/GPT/Claude catalogue.
fn provider_flag(name: &str, default_on: bool) -> bool {
    chosen_models().get("providers").and_then(|p| p.get(name)).and_then(Value::as_bool).unwrap_or(default_on)
}
pub fn zen_enabled() -> bool { provider_flag("zen", false) }
pub fn chinese_models_enabled() -> bool { provider_flag("chinese_models", false) }
/// A WHOLE SEAT SWITCHED OFF (operator 2026-09-30: his Claude subscription ends today and "the router skips a switched-off provider" was promised - but only Zen and the
/// Chinese models had a switch, so Claude would have been tried first on every job until it failed). `providers.seats_off` lists the engine keys the operator turned off;
/// `model_blocked` refuses them, so every router path, the ladder and the alternate picker skip the seat without a call. Default: none off.
pub fn seat_off(engine: Engine) -> bool {
    chosen_models().get("providers").and_then(|p| p.get("seats_off")).and_then(Value::as_array).map(|a| a.iter().any(|k| k.as_str() == Some(engine.key()))).unwrap_or(false)
}
/// ONE MODEL SWITCHED OFF (operator 2026-10-07: "at settings of the agent, to all AIs, add a disable button, so Heimdall won't choose disabled models"). `providers.models_off` lists `engine:model` keys.
/// `model_blocked` refuses them, so every router path - the job ladders, `alternate_seat`, the QA gate, the merge record's winner, a pinned role - skips the model without a call. A key without an effort
/// (`codex:gpt-5.6-terra`) switches off every effort of that model; one with it (`codex:gpt-5.6-terra@high`) only that one. Default: none off.
pub fn model_key(engine: Engine, model: &str) -> String { format!("{}:{}", engine.key(), model.trim()) }
pub fn model_off(engine: Engine, model: &str) -> bool {
    let full = model_key(engine, model).to_ascii_lowercase();
    let base = model_key(engine, model.split('@').next().unwrap_or(model)).to_ascii_lowercase();
    chosen_models().get("providers").and_then(|p| p.get("models_off")).and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).any(|k| { let k = k.trim().to_ascii_lowercase(); k == full || k == base })).unwrap_or(false)
}
pub fn set_model_off(engine: Engine, model: &str, off: bool) -> Result<Value, String> {
    let model = model.trim();
    if model.is_empty() { return Err("a model name is required".into()); }
    let key = model_key(engine, model);
    let mut all = chosen_models();
    if !all.is_object() { all = json!({}); }
    let p = all.as_object_mut().ok_or("bad file")?.entry("providers".to_string()).or_insert_with(|| json!({}));
    let mut list: Vec<Value> = p.get("models_off").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().filter(|k| !k.as_str().map_or(false, |s| s.eq_ignore_ascii_case(&key))).collect();
    if off { list.push(json!(key)); }
    p["models_off"] = json!(list);
    let f = models_file(); if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    std::fs::write(&f, serde_json::to_string_pretty(&all).unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(all)
}
/// THE MODELS A SEAT CAN BE ASKED FOR THAT THE ROUTER KNOWS ABOUT - what Settings lists with a switch each: the tier rungs, the QA gate roster, every job's measured winner and fallback, and whatever is
/// switched off right now (so it can be switched back on). Efforts are folded into the model (`gpt-5.6-terra@medium` -> `gpt-5.6-terra`). Pure over the file and the tables.
pub fn known_models(engine: Engine) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    let mut push = |m: &str| { let m = m.split('@').next().unwrap_or(m).trim(); if !m.is_empty() && !v.iter().any(|x| x == m) { v.push(m.to_string()); } };
    for t in [Tier::Low, Tier::Mid, Tier::High] { if let Some(m) = tier_model(engine, t) { push(&m); } }
    for (e, m) in QA_GATE_ORDER { if *e == engine { push(m); } }
    for j in Job::ALL { let (w, f) = job_defaults(j, engine); push(w); push(f); }
    let prefix = format!("{}:", engine.key());
    if let Some(a) = chosen_models().get("providers").and_then(|p| p.get("models_off")).and_then(Value::as_array) {
        for k in a.iter().filter_map(Value::as_str) { if let Some(m) = k.strip_prefix(&prefix) { push(m); } }
    }
    v
}
/// THE WEEKLY ALLOWANCE THE OPERATOR DECLARES FOR A SEAT THAT PUBLISHES NO WINDOW (operator 2026-10-09: "my Cursor subscription is almost at the limit weekly and this is not shown ... when a subscription gets to
/// its limits you flag it and use the next best"). Cursor, Gemini and Grok publish no quota feed (the CLIs have no usage command), so the only honest percentage is antiloki's OWN list-price estimate of what it
/// sent that seat in the last 7 days against a number the operator sets once (calibrate it by comparing with the seat's own dashboard). `(engine key, dollars)`; empty = nothing tracked.
pub fn allowances() -> Vec<(String, f64)> {
    chosen_models().get("providers").and_then(|p| p.get("allowances")).and_then(Value::as_object)
        .map(|o| o.iter().filter_map(|(k, v)| v.get("week_usd").and_then(Value::as_f64).filter(|x| *x > 0.0).map(|x| (k.clone(), x))).collect()).unwrap_or_default()
}
/// Set (or, with `None`, clear) one seat's weekly allowance in dollars. Only a seat's engine key is accepted; a non-positive number clears it.
pub fn set_weekly_allowance(name: &str, usd: Option<f64>) -> Result<Value, String> {
    let e = engine_of(&json!(name)).ok_or_else(|| format!("unknown seat: {name}"))?;
    let mut all = chosen_models();
    if !all.is_object() { all = json!({}); }
    let p = all.as_object_mut().ok_or("bad file")?.entry("providers".to_string()).or_insert_with(|| json!({}));
    let al = p.as_object_mut().ok_or("bad providers")?.entry("allowances".to_string()).or_insert_with(|| json!({}));
    match usd.filter(|x| x.is_finite() && *x > 0.0) {
        Some(v) => { al[e.key()] = json!({ "week_usd": v }); }
        None => { if let Some(o) = al.as_object_mut() { o.remove(e.key()); } }
    }
    let f = models_file(); if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    std::fs::write(&f, serde_json::to_string_pretty(&all).unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(all)
}
pub fn set_provider_flag(name: &str, on: bool) -> Result<Value, String> {
    // `name` is a switch (zen, chinese_models) or a SEAT's engine key, where `on` is "this seat may be used"
    if let Some(e) = engine_of(&json!(name)) {
        let mut all = chosen_models();
        if !all.is_object() { all = json!({}); }
        let p = all.as_object_mut().ok_or("bad file")?.entry("providers".to_string()).or_insert_with(|| json!({}));
        let mut off: Vec<Value> = p.get("seats_off").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().filter(|k| k.as_str() != Some(e.key())).collect();
        if !on { off.push(json!(e.key())); }
        p["seats_off"] = json!(off);
        let f = models_file(); if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
        std::fs::write(&f, serde_json::to_string_pretty(&all).unwrap_or_default()).map_err(|e| e.to_string())?;
        return Ok(all);
    }
    if !matches!(name, "zen" | "chinese_models") { return Err(format!("unknown provider switch: {name}")); }
    let mut all = chosen_models();
    if !all.is_object() { all = json!({}); }
    let p = all.as_object_mut().ok_or("bad file")?.entry("providers".to_string()).or_insert_with(|| json!({}));
    p[name] = json!(on);
    let f = models_file(); if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    std::fs::write(&f, serde_json::to_string_pretty(&all).unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(all)
}
/// HEIMDALL'S OWN SEAT (operator 2026-09-27: "we should have a main provider for heimdall. On settings as well to
/// select"). The router used to plan on whatever engine the PANE happened to be on — and a roles hand-off had just
/// moved main onto OpenCode with Zen off, so every routed ask died with "OpenCode · Zen is off (Settings › Seats)".
/// The brain is a choice of its own: `providers.heimdall` names the seat that plans, verifies, joins and judges;
/// `"pane"` (the default) keeps the old rule. Either way a blocked or unusable seat is never used — `heimdall_engine`
/// walks to the first usable one instead of failing the ask.
pub fn heimdall_provider() -> String {
    chosen_models().get("providers").and_then(|p| p.get("heimdall")).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("pane").to_string()
}
pub fn set_heimdall_provider(key: &str) -> Result<Value, String> {
    let key = key.trim();
    if key != "pane" && engine_of(&json!(key)).is_none() { return Err(format!("unknown engine for Heimdall: {key} — one of pane · {}", Engine::all().iter().map(|e| e.key()).collect::<Vec<_>>().join(" · "))); }
    let mut all = chosen_models();
    if !all.is_object() { all = json!({}); }
    let p = all.as_object_mut().ok_or("bad file")?.entry("providers".to_string()).or_insert_with(|| json!({}));
    p["heimdall"] = json!(key);
    let f = models_file(); if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    std::fs::write(&f, serde_json::to_string_pretty(&all).unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(all)
}
/// The seat a plan runs on, for a pane on `pane_engine`: the chosen one when usable, else the pane's own when usable,
/// else the first usable seat in the router's order — `None` only when nothing at all can plan. Pure over its inputs.
pub fn heimdall_engine_for(chosen: &str, pane_engine: Option<Engine>, usable: impl Fn(Engine) -> bool) -> Option<Engine> {
    let want = if chosen == "pane" { pane_engine } else { engine_of(&json!(chosen)) };
    if let Some(e) = want { if usable(e) { return Some(e); } }
    if let Some(e) = pane_engine { if usable(e) { return Some(e); } }
    // THE ROUTER'S OWN ORDER, not a list that starts with Claude (operator 2026-10-01, "Engine config also": his Claude subscription has ended and
    // nothing may name it as the answer when the config is empty). `Engine::all()` is the order the router prefers on a free choice.
    Engine::all().into_iter().find(|e| usable(*e))
}
/// The engine a seat text names: `claude-code`, `cli:cursor`, `claude-code:haiku`.
pub fn seat_engine(seat: &str) -> Option<Engine> { engine_of(&json!(seat.trim().trim_start_matches("cli:").split(':').next().unwrap_or(""))) }
/// A SEAT NOBODY NAMED IS NOT CLAUDE'S (operator 2026-11-27: a friend with only Cursor got specialists expecting Claude). The engine an agent starts on: the named one when this machine can use it, else the asking agent's own,
/// else the person's chosen seat, else Claude when usable, else the first usable one. Pure over `usable`.
pub fn seat_for(named: &str, near: Option<Engine>, chosen: &str, usable: impl Fn(Engine) -> bool) -> Engine {
    [seat_engine(named), near, seat_engine(chosen), Some(Engine::ClaudeCode)].into_iter().flatten().find(|e| usable(*e))
        .or_else(|| Engine::all().into_iter().find(|e| usable(*e))).or_else(|| seat_engine(named)).unwrap_or(Engine::ClaudeCode)
}
pub fn agent_seat(named: &str, near: Option<Engine>) -> Engine { seat_for(named, near, &heimdall_provider(), engine_usable) }
/// `cli:<engine>`, or `cli:auto` - the caller names no seat and the serve picks (the FE sends it when the operator pinned nothing; it used to send `cli:claude-code`). Pure over `auto_engine`.
pub fn engine_or_auto(model: &Value) -> Option<Engine> {
    let m = model.as_str().unwrap_or("").trim_start_matches("cli:").trim();
    if m.eq_ignore_ascii_case(ROLE_AUTO) { Some(auto_engine()) } else { engine_of(model) }
}
/// THE SEAT AN UNPINNED ROLE IS RESOLVED AGAINST when no pane names one: the seat Heimdall plans on, else the router's first. Never a hard-coded Claude
/// (the settings screens asked `role_binding(role, Engine::ClaudeCode)`, so a Claude-less machine was shown, and tested, Claude's rungs).
pub fn auto_engine() -> Engine { heimdall_engine(None).unwrap_or(Engine::all()[0]) }
/// A PAIR THE RECORD PICKED MAY STAND ON A SWITCHED-OFF SEAT (operator 2026-09-27: "OpenCode · Zen is off … even
/// though at settings is already selected another provider to heimdall"). The roles record said "Backender on
/// opencode" from three runs measured while Zen was on; with Zen off that pair cannot be built, and the plan died
/// on it AFTER Heimdall had already chosen its own seat. A blocked brain becomes `fallback`; blocked hands become
/// the brain's own (the same engine is a legal pair — the planner does the work). Returns what changed, for the log.
pub fn pair_reachable(p: Pair, fallback: Engine, blocked: impl Fn(Engine, Option<&str>) -> bool) -> (Pair, Option<String>) {
    pair_reachable_why(p, fallback, |e, m| if blocked(e, m) { Some("switched off") } else { None })
}
/// `pair_reachable` with the REASON each side is unreachable (operator 2026-10-01, goals log: "the pair was bent: cursor is switched off" was the only line
/// while a second bend moved the hands again): "switched off" (Settings) or "out of quota" - a seat that is empty until four o'clock is no more a seat for the
/// pair than one that is off, and the log must say which of the two it was.
pub fn pair_reachable_why(mut p: Pair, fallback: Engine, why_not: impl Fn(Engine, Option<&str>) -> Option<&'static str>) -> (Pair, Option<String>) {
    let mut why = vec![];
    if let Some(w) = why_not(p.brain, p.brain_model.as_deref()) { why.push(format!("{} is {w}, the brain is {}", p.brain.key(), fallback.key())); p.brain = fallback; p.brain_model = None; }
    if let Some(w) = why_not(p.hands, p.hands_model.as_deref()) { why.push(format!("{} is {w}, the brain does the work itself", p.hands.key())); p.hands = p.brain; p.hands_model = None; }
    (p, if why.is_empty() { None } else { Some(why.join("; ")) })
}
/// EVERY BEND OF A PAIR, IN ORDER, ENDING WITH WHO DOES THE WORK (operator 2026-10-01). The door bent a pair in two steps - unreachable seats (`pair_reachable_why`),
/// then the frontier rule (`pair_affordable`) - but the log got only the first reason: "the pair was bent: cursor is switched off, the brain does the work itself"
/// while the second step then moved the hands to gemini flash-low. What the run was then doing was not in its log. Returns the pair to use and one line per bend
/// (the reason alone; the caller words the log line), and `work_line` names who does the work in the end. Pure over its inputs - tested.
///
/// The third value is how many of the lines came from the first step (0 or 1): a bench arm is refused on a first-step bend, which is the check the caller needs.
pub fn bend_pair(p: Pair, fallback: Engine, why_not: impl Fn(Engine, Option<&str>) -> Option<&'static str>, cheap: impl Fn(Engine) -> Option<String>,
                 blocked: impl Fn(Engine, Option<&str>) -> bool) -> (Pair, Vec<String>, usize) {
    let mut lines: Vec<String> = vec![];
    let (p, why) = pair_reachable_why(p, fallback, why_not);
    if let Some(w) = why { lines.push(w); }
    let reachable = lines.len();
    let (p, why) = pair_affordable(p, cheap, blocked);
    if let Some(w) = why { lines.push(w); }
    (p, lines, reachable)
}
/// The last line of a bent pair's story: who actually does the work once every bend has happened.
pub fn work_line(p: &Pair) -> String { format!("the pair that does the work: {} - {} decides, {} produces", p.label(), p.brain.key(), p.hands.key()) }
/// A PAIR THE ROUTER PICKED MUST OBEY THE ROUTER'S OWN RULE (found 2026-09-28: every ask on a fresh fork died with
/// HTTP 400 "gpt-5.6-luna@medium and codex are both frontier models", and nothing followed — the operator's
/// "i send a message and nothing happens").
///
/// `problem()` is right that a merge must not buy two frontier prices; refusing the ASK over it is not. The pair
/// arrives from the record or from a role, neither of which consults this rule, so the user is told to "pick a
/// cheaper side" for a pair they never picked. It bends instead, in the order that keeps the most of the intent:
/// the HANDS drop to their own engine's cheap rung (same seat, same conversation, a rung down); failing that they
/// move to the brain's engine and its cheap rung. The brain is never touched — it is the side the plan is judged on.
///
/// Returns the pair to use and what changed, for the run's log. Pure over its inputs — tested.
pub fn pair_affordable(p: Pair, cheap: impl Fn(Engine) -> Option<String>, blocked: impl Fn(Engine, Option<&str>) -> bool) -> (Pair, Option<String>) {
    if p.problem().is_none() { return (p, None) }
    let was = p.label();
    // EVERY CANDIDATE IS CHECKED AGAINST THE GATE IT WILL MEET (audit 2026-09-28). The first version asked only
    // `problem()`, so on a machine with Zen on and Chinese models off it would bend the hands to
    // `opencode/deepseek-v4-flash` — not frontier, so the rule was satisfied — and `cli_command` then refused the
    // call anyway. The ask died one step later than before, under a log line claiming it had been made affordable.
    let ok = |c: &Pair| c.problem().is_none()
        && !blocked(c.brain, c.brain_model.as_deref())
        && !blocked(c.hands, c.hands_model.as_deref());
    // 1. the hands stay on their seat and drop a rung
    if let Some(m) = cheap(p.hands) {
        let cand = Pair { hands_model: Some(m.clone()), ..p.clone() };
        if ok(&cand) { return (cand, Some(format!("{was} would buy two frontier prices — the hands drop to {m}"))) }
    }
    // 2. …or they move to another seat's cheap rung. The brain's own first, then any seat that can take the work:
    //    a merge exists to put cheap hands under a frontier brain, and giving up on that is the LAST resort, not
    //    the second (audit: falling straight to "the brain does it itself" produced the most expensive shape of
    //    all — the frontier model doing every task — and logged it as a saving).
    let mut seats = vec![p.brain];
    seats.extend(Engine::all().into_iter().filter(|e| *e != p.brain));
    for e in seats {
        if let Some(m) = cheap(e) {
            let cand = Pair { hands: e, hands_model: Some(m.clone()), ..p.clone() };
            if ok(&cand) { return (cand, Some(format!("{was} would buy two frontier prices — the hands move to {} {m}", e.key()))) }
        }
    }
    // 3. nothing cheap is reachable anywhere. The brain does the work itself — one seat, one price — and the log
    //    says plainly that this is the fallback and not a bargain.
    let mut q = p.clone();
    q.hands = q.brain; q.hands_model = q.brain_model.clone();
    (q, Some(format!("{was}: no cheap seat is reachable, so the brain does the work itself — this is not a saving")))
}

pub fn heimdall_engine(pane_engine: Option<Engine>) -> Option<Engine> {
    heimdall_engine_for(&heimdall_provider(), pane_engine, |e| engine_routable(e) && model_blocked(e, None).is_none())
}

/// A SEAT THAT HAS RUN OUT OF QUOTA IS NOT A SEAT TODAY (operator 2026-09-30: Codex said "You've hit your usage limit ... try again at 4:44 PM",
/// the planning died on it, and "it should ask an available fallback ... automatically choose the next available best option").
///
/// Nothing remembered the limit, so every later call went to the same empty seat first. `note_limit` remembers it, and the router asks
/// `engine_routable` (usable AND not limited) wherever it picks a seat. It is NOT part of `model_blocked` on purpose: that one means "switched
/// off in Settings", and a seat that is merely empty until four o'clock must not look like a switch the operator flipped.
///
/// HOW LONG: the CLI's own sentence is kept for the screen, and the window is read from it without a time-zone database: a DATE in the sentence
/// ("Oct 9th") is a weekly window, so it is tried again in 6 hours; a bare clock time ("4:44 PM") is a session window, so in 30 minutes. A
/// wrong guess costs one call, because the next failure renews it and the first success clears it.
pub const SESSION_LIMIT_MS: i64 = 30 * 60_000;
pub const WEEKLY_LIMIT_MS: i64 = 6 * 3_600_000;
/// Is this error a QUOTA message — the subscription's own allowance is used up — as opposed to a dead seat, a bad model or a timeout? Pure — tested.
pub fn is_limit_error(why: &str) -> bool {
    let w = why.to_ascii_lowercase();
    ["usage limit", "hit your limit", "rate limit", "rate_limit", "quota exceeded", "exceeded your current quota", "usagelimitexceeded", "insufficient_quota",
     "you've reached your", "limit reached", "resource_exhausted", "resource exhausted", "too many requests", "try again at", "limit will reset", "resets at",
     // Gemini's wording (2026-09-30: "Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 38m26s." killed a run that had another seat to go to)
     "quota reached", "individual quota", "upgrade your subscription", "resets in", "try again in", "daily quota", "out of quota"]
        .iter().any(|k| w.contains(k))
        && !w.contains("context length") && !w.contains("prompt is too long") && !w.contains("max_tokens")
}
/// "Resets in 38m26s" / "try again in 2h 5m" / "retry in 90 seconds": the exact time the message gives, in ms. `None` when it gives none. Pure — tested.
pub fn limit_reset_in_ms(why: &str) -> Option<i64> {
    let w = why.to_ascii_lowercase();
    let at = ["resets in", "reset in", "try again in", "retry in", "retry after", "available in", "again in"].iter().filter_map(|k| w.find(k).map(|i| i + k.len())).min()?;
    let rest = &w[at..];
    let (mut total, mut num, mut any) = (0i64, String::new(), false);
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() { num.push(c); continue }
        if c == ' ' && !num.is_empty() && chars.peek().map_or(false, |n| n.is_ascii_alphabetic()) { continue }
        if !num.is_empty() {
            let n: i64 = num.parse().unwrap_or(0); num.clear();
            let unit: String = std::iter::once(c).chain(chars.clone().take_while(|x| x.is_ascii_alphabetic()).collect::<Vec<_>>()).collect();
            let mult = if unit.starts_with("ms") { 0 } else if unit.starts_with('d') { 86_400_000 } else if unit.starts_with('h') { 3_600_000 } else if unit.starts_with('m') { 60_000 } else if unit.starts_with('s') { 1000 } else { -1 };
            if mult < 0 { if any { break } else { continue } }
            total += n * mult; any = true;
        } else if any && !(c == ' ' || c == ',') && !c.is_ascii_alphabetic() { break }
    }
    if any && total > 0 { Some(total.min(7 * 86_400_000)) } else { None }
}
/// How long a limit message says the seat is out: the exact "resets in ..." when it says one, else a date means a weekly window. Pure — tested.
pub fn limit_window_ms(why: &str) -> i64 {
    if let Some(ms) = limit_reset_in_ms(why) { return ms + 5_000 }
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let w = why.to_ascii_lowercase();
    let dated = MONTHS.iter().any(|m| w.match_indices(m).any(|(i, _)| {
        let before_ok = i == 0 || !w.as_bytes()[i - 1].is_ascii_alphabetic();
        let rest = w[i + m.len()..].trim_start_matches(|c: char| c.is_ascii_alphabetic());
        before_ok && rest.trim_start().chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
    })) || w.contains("weekly") || w.contains("this week") || w.contains("7-day") || w.contains("7 day");
    if dated { WEEKLY_LIMIT_MS } else { SESSION_LIMIT_MS }
}
/// The registry: (scope, engine) -> (until ms, the CLI's own sentence). A type of its own so the rule is testable without the process-wide one.
/// THE SCOPE is 0 in a real process; in a unit-test build it is the test's own thread, so one test marking a seat limited cannot leak into the next.
#[derive(Default)]
pub struct Limits(std::collections::HashMap<(u64, &'static str), (i64, String)>);
impl Limits {
    pub fn note(&mut self, engine: Engine, why: &str, now: i64) { self.note_in(0, engine, why, now) }
    pub fn get(&self, engine: Engine, now: i64) -> Option<(i64, String)> { self.get_in(0, engine, now) }
    pub fn clear(&mut self, engine: Engine) { self.clear_in(0, engine) }
    fn note_in(&mut self, sc: u64, engine: Engine, why: &str, now: i64) { self.0.insert((sc, engine.key()), (now + limit_window_ms(why), why.trim().chars().take(240).collect())); }
    fn get_in(&self, sc: u64, engine: Engine, now: i64) -> Option<(i64, String)> { self.0.get(&(sc, engine.key())).filter(|(until, _)| *until > now).cloned() }
    fn clear_in(&mut self, sc: u64, engine: Engine) { self.0.remove(&(sc, engine.key())); }
}
fn limits_file() -> std::path::PathBuf {
    let home = std::env::var("ANTILOKI_HOME").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::PathBuf::from(".")).join(".antiloki")
    });
    home.join("run").join("limits.json")
}
fn limits() -> &'static std::sync::Mutex<Limits> {
    static L: std::sync::OnceLock<std::sync::Mutex<Limits>> = std::sync::OnceLock::new();
    // A LIMIT SURVIVES A RESTART (2026-09-30: the serve was restarted minutes after Gemini said "resets in 38m", and a memory that lives in RAM would have sent the next job straight back to it).
    // Read once, when the registry is first used; only windows that have not ended are kept. Never in a unit-test build.
    L.get_or_init(|| {
        let mut l = Limits::default();
        #[cfg(not(test))]
        if let Some(v) = std::fs::read_to_string(limits_file()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            for e in Engine::all() {
                if let Some(x) = v.get(e.key()) { if let (Some(until), Some(why)) = (x.get("until").and_then(Value::as_i64), x.get("why").and_then(Value::as_str)) { if until > now_ms() { l.0.insert((0, e.key()), (until, why.to_string())); } } }
            }
        }
        std::sync::Mutex::new(l)
    })
}
#[cfg(not(test))]
fn save_limits(l: &Limits) {
    let mut o = serde_json::Map::new();
    for ((_, k), (until, why)) in l.0.iter() { o.insert((*k).to_string(), json!({ "until": until, "why": why })); }
    let f = limits_file(); if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    let _ = std::fs::write(f, Value::Object(o).to_string());
}
fn limit_scope() -> u64 {
    #[cfg(test)] { use std::hash::{Hash, Hasher}; let mut h = std::collections::hash_map::DefaultHasher::new(); std::thread::current().id().hash(&mut h); return h.finish() | 1; }
    #[cfg(not(test))] { 0 }
}
/// An engine answered with a quota message: remember it. Returns true when it was a limit (so the caller can say so in the run's log).
pub fn note_limit(engine: Engine, why: &str) -> bool {
    // a LAPSED SUBSCRIPTION looks like "not logged in" / "no credit", not "usage limit": the seat is just as unavailable, and trying it first on every job costs a failed call each time
    if !is_limit_error(why) && !crate::gauge::seat_dead(why).sure { return false }
    { let mut l = limits().lock().unwrap_or_else(|p| p.into_inner()); l.note_in(limit_scope(), engine, why, now_ms()); #[cfg(not(test))] save_limits(&l); }
    true
}
/// A call on this engine just worked: whatever limit we remembered is over.
pub fn clear_limit(engine: Engine) {
    let mut l = limits().lock().unwrap_or_else(|p| p.into_inner());
    if l.get_in(limit_scope(), engine, now_ms()).is_none() { return }       // nothing remembered: nothing to write
    l.clear_in(limit_scope(), engine); #[cfg(not(test))] save_limits(&l);
}
/// THE WINDOWS ARE CHECKED BEFORE THE JOB, NOT AFTER IT FAILS (operator 2026-09-30: "when choosing a model for the job before even start it should check limits to
/// see it will be enough"). The subscriptions report how full their 5-hour and weekly windows are (Claude's meter, Codex's session files - `fuel::codex_windows`);
/// a seat whose window is at `WINDOW_FULL_PCT` or more is treated as out until that window resets, exactly like one that already answered "usage limit", so every
/// router path - the plan's seat, each job's winner and fallback, the QA, the alternate - skips it BEFORE the call. The figure is a percentage of a window whose
/// size in tokens the CLIs do not publish, so "enough" cannot be computed in tokens: the rule is "not nearly full". The data is re-read at most every 30 seconds.
pub const WINDOW_FULL_PCT: f64 = 95.0;
const WINDOWS_TTL_MS: i64 = 30_000;
#[derive(Clone, Debug, PartialEq)]
pub struct Window { pub house: String, pub label: String, pub used_pct: f64, pub resets_at_s: Option<i64> }
fn windows() -> &'static Mutex<(i64, Vec<Window>)> { static W: OnceLock<Mutex<(i64, Vec<Window>)>> = OnceLock::new(); W.get_or_init(Default::default) }
/// Keep the windows a reader just produced (`agents_http::account_windows` shape: house/label/used_pct/resets_at).
pub fn set_windows(v: &[Value]) {
    let ws: Vec<Window> = v.iter().filter_map(|w| Some(Window {
        house: w.get("house")?.as_str()?.to_string(), label: w.get("label")?.as_str()?.to_string(),
        used_pct: w.get("used_pct")?.as_f64()?, resets_at_s: w.get("resets_at").and_then(Value::as_i64) })).collect();
    *windows().lock().unwrap_or_else(|p| p.into_inner()) = (now_ms(), ws);
}
/// Re-read the windows when they are older than the TTL. `root` is the workspace whose Claude meter is read.
pub fn refresh_windows(root: &str) {
    let stale = { let g = windows().lock().unwrap_or_else(|p| p.into_inner()); now_ms() - g.0 > WINDOWS_TTL_MS };
    // (not in a test build: it would read THIS machine's session files, and a test must not depend on how full the developer's Codex window is)
    #[cfg(not(test))] if stale && !root.trim().is_empty() { let w = crate::agents_http::account_windows(root); set_windows(&w); }
    #[cfg(test)] { let _ = (stale, root); }
}
/// The fullest live window of an engine's house as `(used %, sentence, resets at ms)`, when it is at `WINDOW_FULL_PCT` or more. Pure over the windows - tested.
pub fn full_window(ws: &[Window], engine: Engine, now_s: i64) -> Option<(f64, String, i64)> {
    let house = crate::fuel::house_of(engine.key());
    ws.iter().filter(|w| w.house == house && w.used_pct >= WINDOW_FULL_PCT && w.resets_at_s.map(|r| r > now_s).unwrap_or(true))
        .max_by(|a, b| a.used_pct.total_cmp(&b.used_pct))
        .map(|w| (w.used_pct, format!("{} {} window is {}% used{}", w.house, if w.label == "5h" { "5-hour" } else if w.label == "week" { "weekly" } else { w.label.as_str() }, w.used_pct.round(),
            w.resets_at_s.map(|r| format!(" - it resets in {} min", ((r - now_s).max(0) + 59) / 60)).unwrap_or_default()),
            w.resets_at_s.map(|r| r * 1000).unwrap_or(now_s * 1000 + SESSION_LIMIT_MS)))
}
/// `Some((until_ms, the reason))` while the seat cannot take a call: it said it is out of quota, or its window is nearly full.
pub fn seat_limit(engine: Engine) -> Option<(i64, String)> {
    if let Some(l) = limits().lock().unwrap_or_else(|p| p.into_inner()).get_in(limit_scope(), engine, now_ms()) { return Some(l) }
    let ws = windows().lock().unwrap_or_else(|p| p.into_inner()).1.clone();
    full_window(&ws, engine, now_ms() / 1000).map(|(_, why, until)| (until, why))
}
/// Usable AND has quota left: the question the router asks when it CHOOSES a seat.
pub fn engine_routable(engine: Engine) -> bool { engine_usable(engine) && seat_limit(engine).is_none() }

/// The families the Chinese switch covers. Matched on the model id, so it holds whichever engine serves it.
pub fn is_chinese_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    ["deepseek", "qwen", "kimi", "moonshot", "glm", "zhipu", "minimax", "ernie", "hunyuan", "baichuan", "yi-", "stepfun"]
        .iter().any(|f| m.contains(f))
}
/// WHY THIS CALL MAY NOT BE MADE — `None` when it may. The ONE gate; every path that picks a model asks it, and
/// `cli_command` asks it again before an argv is built, so a path that forgot cannot leak a call through.
/// An OpenCode model that does not bill through Zen: the Go subscription or a model on this machine (operator 2026-11-27, who holds Go: the switch blocked those too). No model named: OpenCode's own default,
/// which may be Zen, so it does not count. Every other provider prefix stays blocked while Zen is off (their keys are metered). Pure.
pub fn not_through_zen(model: &str) -> bool { let m = model.trim().to_ascii_lowercase(); ["opencode-go/", "ollama/"].iter().any(|p| m.strip_prefix(p).is_some_and(|r| !r.is_empty())) }
/// May this member's call be made: as `engine_usable`, except that OpenCode on a model that does not go through Zen needs only to be installed and switched on.
pub fn member_usable(engine: Engine, model: Option<&str>) -> bool { if engine.is_metered_gateway() && model.is_some_and(not_through_zen) { return !seat_off(engine) && engine_available(engine); } engine_usable(engine) }
pub fn model_blocked(engine: Engine, model: Option<&str>) -> Option<String> {
    if seat_off(engine) { return Some(format!("{} is switched off (Settings › Seats).", provider_name(engine))); }
    if let Some(m) = model.filter(|m| !m.trim().is_empty()) {
        if model_off(engine, m) { return Some(format!("{m} is switched off (Settings › Seats).")); }
    }
    if engine.is_metered_gateway() && !zen_enabled() && !model.is_some_and(not_through_zen) {
        return Some("OpenCode · Zen is off. It bills metered credit, so OpenCode runs in the background only on a model that does not go through it: one of your Go plan (opencode-go/...) or a local one \
                     (ollama/...). Name that model, or turn Zen on if you hold Zen credit.".into());
    }
    if let Some(m) = model {
        if is_chinese_model(m) && !chinese_models_enabled() {
            return Some(format!("{m} is a Chinese model and they are off (Settings › Seats)."));
        }
        // CURSOR SERVES ITS OWN MODELS FROM A DIFFERENT POOL THAN EVERYONE ELSE'S (operator 2026-09-27: "use cursor's
        // own on cursor as default, never the third party"). Composer 2.5 and the `cursor-`prefixed Grok builds draw
        // on the plan's own-model pool — generous, permanently doubled, and Composer carries no token rate at all.
        // Claude, GPT, Gemini and Grok 4.7 (the Grok WITHOUT the prefix) bill at API rates against the $20/month
        // third-party allowance: the pool that empties, and the one a long run drains without warning. Reaching a
        // Claude model through Cursor also pays twice for a seat the operator already holds.
        // A GUARD, NOT A SWITCH — the instruction was "never", so there is no toggle for this one.
        if engine == Engine::Cursor && !is_cursor_own_model(m) {
            return Some(format!("{m} is not one of Cursor's own models: it bills at API rates against Cursor's \
                                 $20/month third-party allowance. Use composer-2.5 or a cursor-grok-* build."));
        }
        // the same rule for Antigravity, which proxies Claude and GPT alongside Google's own models
        if engine == Engine::Gemini && !is_gemini_own_model(m) {
            return Some(format!("{m} is not one of Google's own models: reaching it through Antigravity spends a \
                                 Gemini allowance on another vendor's model. Use a gemini-* model."));
        }
    }
    None
}
/// WHY THIS SIDE OF A ROW IS NOT ON A SUBSCRIPTION — `None` when it is (operator 2026-09-27: "we only use
/// subscriptions, that is the moat of antiloki. So opencode only go subscription, cursor their own not thirdparties,
/// gemini as well").
///
/// Takes the record's own spelling of a side, `"<engine>:<model>"` (`"claude-code:sonnet"`,
/// `"opencode:opencode/deepseek-v4-flash"`, `"cursor:composer-2.5"`), plus the bare markers a bench writes for an
/// absent side (`"raw"`, `"-"`, `""`), which are never metered. Used to mark the legacy archive and to keep a metered
/// result out of the live table — it is a property of the ROW, so it must be answerable from the stored string alone.
pub fn side_is_metered(side: &str) -> Option<String> {
    let t = side.trim();
    if t.is_empty() || t == "-" || t == "raw" || t == "solo" { return None; }
    let (eng, model) = t.split_once(':').map(|(a, b)| (a, b.trim())).unwrap_or((t, ""));
    let engine = engine_of(&json!(eng));
    let m = if model.is_empty() { None } else { Some(model) };
    // a Chinese model is only ever reached here through a metered gateway, whichever engine names it
    if let Some(mm) = m { if is_chinese_model(mm) { return Some(format!("{mm} is reached through metered credit, not a subscription")); } }
    match engine {
        // EVERY model OpenCode serves is on the Zen gateway (`opencode models` lists nothing outside the
        // `opencode/` namespace), and Zen bills a credit balance. The Go plan does not change that: the 402 came
        // from Zen's own upstream, not from the subscription.
        Some(Engine::Opencode) => Some("OpenCode · Zen bills metered credit, not a subscription".into()),
        Some(Engine::Cursor) => match m {
            Some(mm) if !is_cursor_own_model(mm) => Some(format!("{mm} bills at API rates against Cursor's third-party allowance")),
            // an unnamed Cursor side resolves to `auto`, which may pick from that same allowance
            None => Some("an unnamed Cursor model resolves to `auto`, which may bill the third-party allowance".into()),
            _ => None,
        },
        _ => None,
    }
}

/// GOOGLE'S OWN MODELS ONLY (operator 2026-09-27: "cursor their own not thirdparties, gemini as well"). `agy models`
/// on this account also lists `claude-sonnet-4-6`, `claude-opus-4-6-thinking` and `gpt-oss-120b-medium` — Antigravity
/// proxies other vendors exactly as Cursor does, and reaching Claude through Google's quota spends a Gemini allowance
/// on a model the operator already holds a Claude seat for. Gemini's own models carry the `gemini-` prefix.
/// IS THIS MODEL THIS SEAT'S OWN — the question `model_blocked` answers only for the two seats that proxy other
/// vendors. A pane that switches engines carries its old model with it, and a Claude model handed to Codex is not
/// refused by anything above: it is simply wrong. Cheap, syntactic, and deliberately generous — an id this cannot
/// place is allowed, because a new model name must never become an outage. Pure — tested.
pub fn model_is_of(engine: Engine, model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() { return true }
    // the family names Anthropic and OpenAI actually ship, not a guess: "claude-fable-5-1" and "fable" are as
    // much Claude models as "sonnet" is (audit 2026-09-28 — this file names one three hundred lines below), and
    // OpenAI's reasoning line has run o1/o3/o4/o5
    let claude = m.contains("claude")
        || ["opus", "sonnet", "haiku", "fable", "mythos"].iter().any(|f| m.starts_with(f));
    let gpt = m.starts_with("gpt") || m.contains("codex")
        || (m.starts_with('o') && m.as_bytes().get(1).map(|c| c.is_ascii_digit()).unwrap_or(false));
    match engine {
        Engine::ClaudeCode => claude,
        // Codex's tiers are a reasoning effort on the account's own model, so `medium`/`high` are legal ids here
        Engine::Codex => gpt || matches!(m.as_str(), "low" | "medium" | "high" | "minimal"),
        Engine::Grok => m.contains("grok") || matches!(m.as_str(), "low" | "medium" | "high"),
        Engine::Cursor => is_cursor_own_model(&m),
        Engine::Gemini => is_gemini_own_model(&m),
        // OpenCode is the gateway: it serves everyone's models, which is the whole point of it
        Engine::Opencode => true,
    }
}

/// `default` (or no name) is Antigravity's OWN default — a Google model. Refusing it bent every `gemini:default` planner to Claude
/// Sonnet, silently, on 2026-09-29 (the S2 gemini arms were all planned by Sonnet). Pure — tested.
pub fn is_gemini_own_model(model: &str) -> bool { let m = model.trim().to_ascii_lowercase(); m.is_empty() || m == "default" || m.starts_with("gemini-") }

/// Is this model served from Cursor's OWN pool? Composer, and the `cursor-`prefixed builds Cursor serves itself.
/// `auto` is not: it may resolve to anything, including the allowance that empties.
pub fn is_cursor_own_model(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.starts_with("composer") || m.starts_with("cursor-")
}
/// Is this engine allowed at all right now — seat, binary, and the provider switches.
pub fn provider_allowed(engine: Engine) -> bool { model_blocked(engine, None).is_none() }

/// Installed AND signed in — the only question onboarding and the router should ask.
pub fn engine_usable(engine: Engine) -> bool {
    // provider_allowed FIRST: a switched-off provider is not usable even with a seat and a binary, and this is the
    // chokepoint every router path already asks (2026-09-27 — the 402 reached the record because nothing asked).
    provider_allowed(engine) && engine_available(engine) && engine_seat(engine) != Some(false)
}

/// THE "IS IT INSTALLED" ANSWERS, per seat: installed for the life of the process, NOT installed for 10 s (see `engine_available`).
fn seen_binaries() -> &'static std::sync::Mutex<std::collections::HashMap<Engine, (bool, std::time::Instant)>> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<Engine, (bool, std::time::Instant)>>> = std::sync::OnceLock::new();
    SEEN.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}
/// Forget what was seen on the PATH, so the next question walks it again. For a caller that has just changed the PATH or the seat's login (the journeys do: a "not installed" answer is
/// otherwise good for 10 s and a journey that starts inside that window saw the previous journey's PATH, which made j20 pass alone and fail after j19 on 2026-10-09).
pub fn forget_seat_probes() { if let Ok(mut g) = seen_binaries().lock() { g.clear(); } }
pub fn engine_available(engine: Engine) -> bool {
    #[cfg(test)] { let _ = engine; return true; }
    #[cfg(not(test))] {
        // "Installed" is cached for the life of the process; "NOT installed" only for 10 s (audit 2026-10-05: it was
        // cached forever, so onboarding's "install the seat" step did nothing until serve restarted). A PATH walk is cheap.
        let m = seen_binaries();
        if let Some((v, at)) = m.lock().ok().and_then(|g| g.get(&engine).copied()) { if v || at.elapsed() < std::time::Duration::from_secs(10) { return v; } }
        // any binary that reaches the seat counts — Gemini is `agy` OR the legacy `gemini`
        let ok_any = engine.bins().iter().any(|b| crate::proc::which(b).is_some());
        // `which` is not a program on Windows, so this answered "not installed" for every engine on a machine
        // that had all three. proc::which walks the PATH in process — no child, no console flash, and PATHEXT
        // so that an npm-installed `claude.cmd` is found when the caller asked for `claude`.
        let ok = ok_any;
        if let Ok(mut g) = m.lock() { g.insert(engine, (ok, std::time::Instant::now())); }
        ok
    }
}
/// THE ROUTER'S PICK FOR A JOB, across providers: the operator's global cell (`jobs.<job>.any = {winner:{engine,model},
/// fallback:{…}}`) or the measured global winner, each only when its provider is installed here — otherwise the run
/// engine's own cell. The models bar shifts the pick like a role. Pure over the file and the `which` cache.
pub fn job_binding_any(job: Job, run_engine: Engine, cost: Option<u8>) -> (Binding, Binding) {
    let cell = chosen_models().get("jobs").and_then(|j| j.get(job.key())).and_then(|j| j.get("any")).cloned().unwrap_or(Value::Null);
    let read = |k: &str, d: (Engine, &'static str)| -> (Engine, String, &'static str) {
        let c = cell.get(k);
        match (c.and_then(|x| x.get("engine")).and_then(|e| engine_of(e)), c.and_then(|x| x.get("model")).and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty())) {
            (Some(e), Some(m)) => (e, m.to_string(), "job"),
            _ => (d.0, d.1.to_string(), "measured"),
        }
    };
    let (gw, gf) = job_global(job);
    let (we, wm, ws) = read("winner", gw); let (fe, fm, fs) = read("fallback", gf);
    let step: i8 = match cost { Some(0) => -1, Some(2) => 1, _ => 0 };
    let shift = |e: Engine, m: String| if step == 0 || e == Engine::Codex { m } else { shift_model(e, Some(&m), Tier::Mid, step).unwrap_or(m) };
    let local = job_binding(job, run_engine, cost);
    let winner = if engine_usable(we) { Binding { engine: we, model: named_model(shift(we, wm)), source: ws } } else { local.0.clone() };
    let fallback = if engine_usable(fe) && (fe, &fm) != (winner.engine, winner.model.as_ref().unwrap_or(&String::new())) { Binding { engine: fe, model: named_model(shift(fe, fm)), source: fs } } else { local.1 };
    (winner, fallback)
}
/// THE PAIR — THE MERGE (operator 2026-09-22: "merge models into specialized coding agents"; "the agents merge inside
/// Studio › agents"). Two engines make one agent: the BRAIN does the jobs that decide (planner, verifier, QA gate,
/// test-writer, synthesis, screening), the HANDS do the jobs that produce (editor-s, editor-l, lookup, report).
/// A pair may name the model each side runs; without one, the side's own job cell applies. Stored on the pane as
/// `settings.pair = {name, brain: {engine, model?}, hands: {engine, model?}}`; `name` alone picks a built-in.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pair { pub name: String, pub brain: Engine, pub hands: Engine, #[serde(default)] pub brain_model: Option<String>, #[serde(default)] pub hands_model: Option<String> }
impl Pair {
    /// the built-ins the menu offers — measured (Claude × DeepSeek), or named for the bench that comes next
    pub fn builtin(name: &str) -> Option<Pair> {
        match name.trim().to_ascii_lowercase().as_str() {
            // ── THE SUBSCRIPTION MERGES ──────────────────────────────────────────────────────────────────────
            // SUBSCRIPTIONS ARE THE MOAT (operator 2026-09-27: "we only use subscriptions, that is the moat of
            // antiloki"). Every side below is a flat monthly seat the operator already holds, so the merge costs
            // nothing per task — which is the whole claim. Nothing here touches a metered balance: no Zen, no
            // Cursor third-party model, no API key.
            //
            // The hands are always strictly cheaper than the brain (`worker_under_planner` enforces it, comparing
            // `price_of` — the output rung in $/1M shown after each name).
            "claude-composer" | "claude × composer" => Some(Pair { name: "claude-composer".into(),
                brain: Engine::ClaudeCode, hands: Engine::Cursor,
                brain_model: Some("sonnet".into()), hands_model: Some("composer-2.5".into()) }),          // 15.00 → 1.50
            // "CLAUDE GROK" IS ONE OF THE TWO NAMES IN THE HEADLINE, so it is one of the two that must end up measured.
            "claude-grok" | "claude × grok" | "claudegrok" => Some(Pair { name: "claude-grok".into(),
                brain: Engine::ClaudeCode, hands: Engine::Cursor,
                brain_model: Some("opus".into()), hands_model: Some("cursor-grok-4.6-medium".into()) }),   // 25.00 → 15.00
            "claude-gemini" | "claude × gemini" => Some(Pair { name: "claude-gemini".into(),
                brain: Engine::ClaudeCode, hands: Engine::Gemini,
                brain_model: Some("sonnet".into()), hands_model: Some("gemini-3.8-flash-low".into()) }),    // 15.00 → 0.28
            "claude-codex" | "claude × codex" => Some(Pair { name: "claude-codex".into(),
                brain: Engine::ClaudeCode, hands: Engine::Codex,
                brain_model: Some("opus".into()), hands_model: Some("low".into()) }),                      // 25.00 → 10.00 · low effort
            // "GEMINI CODEX" IS THE OTHER HEADLINE NAME. The hands are the cheaper side, so Codex plans and Gemini
            // writes — the name reads brain-first but the rule decides which side is which.
            "codex-gemini" | "codex × gemini" | "geminicodex" | "gemini-codex" => Some(Pair { name: "codex-gemini".into(),
                brain: Engine::Codex, hands: Engine::Gemini,
                brain_model: Some("medium".into()), hands_model: Some("gemini-3.8-flash-low".into()) }),   // 10.00 → 0.28
            "codex-composer" | "codex × composer" => Some(Pair { name: "codex-composer".into(),
                brain: Engine::Codex, hands: Engine::Cursor,
                brain_model: Some("medium".into()), hands_model: Some("composer-2.5".into()) }),           // 10.00 → 1.50
            "gemini-composer" | "gemini × composer" => Some(Pair { name: "gemini-composer".into(),
                brain: Engine::Gemini, hands: Engine::Cursor,
                brain_model: None, hands_model: Some("composer-2.5".into()) }),                            // 10.00 → 1.50
            "grok-composer" | "grok × composer" => Some(Pair { name: "grok-composer".into(),
                brain: Engine::Cursor, hands: Engine::Cursor,
                brain_model: Some("cursor-grok-4.6-high".into()), hands_model: Some("composer-2.5".into()) }), // 15.00 → 1.50
            "grok-codex" | "grok × codex" => Some(Pair { name: "grok-codex".into(),
                brain: Engine::Cursor, hands: Engine::Codex,
                brain_model: Some("cursor-grok-4.6-high".into()), hands_model: Some("low".into()) }),       // 15.00 → 10.00 · low effort
            _ => None,
        }
    }
    /// THE RETIRED MERGES — DISCONTINUED, NOT DELETED (operator 2026-09-27: "remove all zen merges completely, add to
    /// a legacy benchmark discontinued and not visible by the user").
    ///
    /// All six built-ins antiloki shipped until today routed through OpenCode Zen, which bills metered credit rather
    /// than a subscription and returned HTTP 402 for a whole day. `builtin()` no longer returns any of them, so none
    /// can be selected, launched or recorded against. Their MEASUREMENTS are kept — 532 samples, and the
    /// `source='use'` rows are observed production evidence that cannot be re-created — but they are marked legacy in
    /// the record and never shown to the operator or used for routing (see `merges::migrate_legacy`).
    pub const LEGACY_BUILTINS: [&'static str; 6] = ["claude-deepseek", "kimi-deepseek", "claude-qwen", "claude-glm", "codex-deepseek", "deepseek-deepseek"];
    /// Was this pair name one of the retired Zen merges? Used to label the archive, never to resolve a pair.
    pub fn is_legacy_name(name: &str) -> bool {
        let n = name.trim().to_ascii_lowercase();
        Self::LEGACY_BUILTINS.iter().any(|l| *l == n) || n.contains("deepseek") || n.contains("qwen") || n.contains("glm") || n.contains("kimi")
    }
    pub const BUILTINS: [&'static str; 9] = ["claude-composer", "claude-grok", "claude-gemini", "claude-codex",
                                             "codex-gemini", "codex-composer", "gemini-composer", "grok-composer", "grok-codex"];
    /// the pane's `settings.pair`, a name or the full shape; None = solo (no merge)
    pub fn of_settings(v: &Value) -> Option<Pair> {
        let p = v.get("pair")?;
        if let Some(n) = p.as_str() { return if n.trim().is_empty() || n == "solo" { None } else { Pair::builtin(n) }; }
        let name = p.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
        if name == "solo" { return None; }
        // TWO SHAPES, because a Pair has to survive its own round trip (2026-09-24). The FE sends
        // `{"brain": {"engine": "...", "model": "..."}}`; serialising the parsed Pair back out gives
        // `{"brain": "claude-code", "brain_model": "sonnet"}` — and only the first was understood. So a merge
        // stored on a recipe came back as `solo` the moment it was read from a pane's settings, which is what the
        // roster and the settings screen both do.
        let model_at = |key: &str| p.get(key).and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()).map(String::from);
        let side = |k: &str| -> Option<(Engine, Option<String>)> {
            let v = p.get(k)?;
            if v.is_string() { return Some((engine_of(v)?, model_at(&format!("{k}_model")))); }
            let e = engine_of(v.get("engine").unwrap_or(&Value::Null))?;
            Some((e, v.get("model").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()).map(String::from)))
        };
        match (side("brain"), side("hands")) {
            (Some((be, bm)), Some((he, hm))) => Some(Pair { name: if name.is_empty() { format!("{}-{}", be.key(), he.key()) } else { name }, brain: be, hands: he, brain_model: bm, hands_model: hm }),
            _ => if name.is_empty() { None } else { Pair::builtin(&name) },
        }
    }
    /// A FRONTIER MODEL (operator 2026-09-23: "never mix frontier models, always a frontier + cheaper, or 2 cheapers"):
    /// the big labs' own subscription models. A merge of two of them buys the price of both and the speed of the
    /// slower — the whole point of a pair is that one side is cheap. Claude Haiku and everything on the OpenCode
    /// gateway are the cheap side, unless the gateway is serving a frontier model by name. Pure — tested.
    pub fn is_frontier(engine: Engine, model: Option<&str>) -> bool {
        let m = model.unwrap_or("").to_ascii_lowercase();
        match engine {
            // CODEX'S TIERS ARE EFFORTS, AND LOW EFFORT IS ITS CHEAP RUNG (2026-09-27). This returned `true`
            // unconditionally, which made every Codex side frontier and so refused `claude-codex` and `grok-codex` as
            // "two frontier sides" — the operator asked for Codex in the pairs, and a low-effort Codex worker under an
            // Opus or Grok brain is exactly the frontier-brain/cheap-hands shape the rule exists to require.
            Engine::Codex => !matches!(codex_model_effort(&m).1.as_deref(), Some("low")),
            Engine::ClaudeCode => !m.contains("haiku"),
            Engine::Opencode => m.contains("gpt") || m.contains("claude") || m.contains("opus") || m.contains("sonnet"),
            // the cheap rung of each new seat is its fast model; anything else on it counts as frontier
            // the flash rungs are Gemini's cheap side; `pro` is not. An unnamed side resolves to the account default,
            // which cannot be assumed cheap.
            Engine::Gemini => !(m.contains("flash") || m.contains("lite")),
            Engine::Grok => !(m.contains("fast") || m.contains("mini") || m.contains("code")),
            // CURSOR'S OWN POOL IS NOT A SECOND BILL (2026-09-27). This rule exists so a merge does not "buy both
            // prices"; a model Cursor serves itself carries NO token rate against the plan's own-model pool, so it is
            // the cheap side by construction — Composer and the `cursor-grok-*` builds alike. `list_price` still
            // carries Grok's open-market rate, because that is the yardstick a SAVING is measured against, but paying
            // twice is a different question from what a model would cost elsewhere.
            // Anything outside that pool bills at API rates and is frontier — and `model_blocked` refuses it anyway.
            Engine::Cursor => !is_cursor_own_model(&m),
        }
    }
    /// Why this pair may not be used, if it may not. Two frontier sides is the only refusal — see `is_frontier`. Pure.
    pub fn problem(&self) -> Option<String> {
        // ONE MODEL ON BOTH SIDES IS NOT A MERGE (2026-09-28). The rule exists so a merge does not buy two
        // frontier prices; a pair whose sides are the same seat and the same model buys ONE — it is a solo run
        // wearing a pair's shape, which is exactly what a bend falls back to when nothing cheap is reachable.
        // Without this, that fallback refused itself and the ask died with a 400 nobody could act on.
        if self.brain == self.hands && self.brain_model == self.hands_model { return None }
        let (b, h) = (Self::is_frontier(self.brain, self.brain_model.as_deref()), Self::is_frontier(self.hands, self.hands_model.as_deref()));
        if b && h {
            let name = |e: Engine, m: &Option<String>| m.clone().unwrap_or_else(|| e.key().to_string());
            return Some(format!("{} and {} are both frontier models — a merge pairs a frontier brain with cheap hands, or two cheap models; pick a cheaper side", name(self.brain, &self.brain_model), name(self.hands, &self.hands_model)));
        }
        None
    }
    /// which side a job belongs to
    pub fn side(job: Job) -> &'static str { match job { Job::Planner | Job::Verifier | Job::QaGate | Job::TestWriter | Job::Synthesis | Job::Screening => "brain", _ => "hands" } }
    /// the human line: "Claude × DeepSeek"
    /// THE NAME OF A PAIR: the provider AND the model on each side (operator 2026-09-23: "all merges call the name of
    /// the provider + model — never just provider, never just model"). A side whose model the job decides says so.
    pub fn label(&self) -> String { format!("{} × {}", side_name(self.brain, self.brain_model.as_deref()), side_name(self.hands, self.hands_model.as_deref())) }
}
/// THE ROUTER READS THE MERGE TABLE (2026-09-23): for a solo run of a known size, a combination the table has seen at
/// least three times outranks the static jobs table — its side for this job is the winner, the static pick the
/// fallback. Under a pair the pair rules; with too few samples the static table rules. Pure over the two stores.
/// DOES A MERGE SPLIT AT THIS SIZE (operator 2026-09-28: "a small task like add a button is getting split into
/// brain and worker. But we said only L and above gets the split merge and all below gets only the worker + QA")?
///
/// A merge buys supervision: a dearer brain decides, a cheaper hand writes. That trade only pays when there is
/// something to decide. On a one-file, one-line ask the planning IS the ask, so the split adds a planner's round
/// trip — measured at 24 to 48 times the worker's price for the same job — and a second model's opinion about a
/// button. The operator watched exactly that happen and said so.
///
/// So the pair applies at `l` and above. Below it the hands do the work alone and QA still judges it, which is why
/// `QaGate` is exempt: it is not the merge's brain, it is the check that the merge was right, and a small task
/// wants it just as much. Pure — tested.
pub fn pair_splits_at(size: &str) -> bool { matches!(size.trim().to_ascii_lowercase().as_str(), "l" | "xl" | "xxl") }

pub fn job_binding_learned(job: Job, size: &str, pair: Option<&Pair>, run_engine: Engine, cost: Option<u8>) -> (Binding, Binding) {
    job_binding_learned_in(job, size, pair, run_engine, cost, None)
}
/// THE EDITOR BINDING KNOWS ITS DOMAIN (PLAN-FINISH-THE-BENCHMARKS P2, 2026-09-28). The editor is bound AFTER the tasks exist, so the
/// files already say what kind of work it is — and `merges::best` answered the same pairing for every domain while the domain ladder
/// (`best_for_role`, what roles.rs reads) held a different winner in five of six (the auditor's "two winner lookups disagree").
/// With a domain, an EDITING job asks the domain ladder first; everything else — the brain jobs, which are chosen before any file
/// exists, and a run with no domain — keeps the blind lookup. A pair is untouched: the operator's choice of engine is absolute.
pub fn learned_winner(job: Job, size: &str, domain: Option<&str>) -> Option<(Engine, String)> {
    learned_pick(job, size, domain).map(|(e, m, _)| (e, m))
}
/// …AND WHAT THE RECORD SAID ABOUT IT. At the sizes that need no planner (s, l) an EDITING job's winner is chosen among the
/// solo cells and the pairs together (`merges::best_cell`, item 4 of the 2026-10-01 queue): the seat that wins may be one
/// that was measured alone, and the third element is the sentence that says what the solo cells did to the answer (empty
/// when they were not part of the contest). Without a domain the pool is every domain — the same blind read as before,
/// ranked by the rule. Every other job keeps the blind lookup. Pure over the record — tested.
pub fn learned_pick(job: Job, size: &str, domain: Option<&str>) -> Option<(Engine, String, String)> {
    if matches!(job, Job::EditorS | Job::EditorL) {
        let editor = if size == "s" { "editor-s" } else { "editor-l" };
        let d = domain.filter(|d| !d.is_empty() && !matches!(*d, "any" | "unknown" | "*")).unwrap_or("*");
        if let Some(p) = crate::merges::best_cell(editor, size, d, crate::roles::Objective::Best, 3) {
            let w = p.worker.as_str();
            if w != "raw" && w != "-" { if let Some((e, m)) = w.split_once(':') { if let Some(eng) = engine_of(&json!(e)) { return Some((eng, m.to_string(), p.note)) } } }
        }
    }
    crate::merges::best_for_in(job, size, domain, 3).map(|(e, m)| (e, m, String::new()))
}
pub fn job_binding_learned_in(job: Job, size: &str, pair: Option<&Pair>, run_engine: Engine, cost: Option<u8>, domain: Option<&str>) -> (Binding, Binding) {
    // A PAIR USED TO SWITCH THE RECORD OFF ENTIRELY (2026-09-26). This early return dropped `size`, and
    // `job_binding_paired` falls through to `job_defaults`, which is size-blind — so under a merge NOTHING the record
    // learned was consulted and a one-line fix was planned as expensively as a subsystem.
    //
    // The early return itself is right: `best_for` returns the globally best (engine, model), and consulting it under
    // a pair would silently override the provider the operator chose. So the winner is asked for WITHIN the side's own
    // engine — it may move the model that side runs, never the engine. The pair's choice is still absolute.
    if let Some(p) = pair {
        // …BUT NOT ON SMALL WORK. The brain half is dropped below `l` and the hands do it alone; QA keeps its own
        // judge, because the check is not the thing being merged. See `pair_splits_at`.
        if !pair_splits_at(size) && Pair::side(job) == "brain" && job != Job::QaGate {
            let hands = Pair { name: p.name.clone(), brain: p.hands, hands: p.hands,
                               brain_model: p.hands_model.clone(), hands_model: p.hands_model.clone() };
            return job_binding_paired(job, Some(&hands), run_engine, cost);
        }
        let (w, f) = job_binding_paired(job, pair, run_engine, cost);
        let side_engine = if Pair::side(job) == "brain" { p.brain } else { p.hands };
        // only when the pair did not NAME a model for that side: a named model is an instruction, not a default
        let named = if Pair::side(job) == "brain" { p.brain_model.is_some() } else { p.hands_model.is_some() };
        if !named {
            if let Some(m) = crate::merges::best_model_within(job, size, side_engine, 3) {
                if Some(m.as_str()) != w.model.as_deref() {
                    return (Binding { engine: side_engine, model: Some(m), source: "learned" }, w);
                }
            }
        }
        return (w, f);
    }
    let (sw, sf) = job_binding_any(job, run_engine, cost);
    match learned_winner(job, size, domain) {
        Some((e, m)) if engine_usable(e) && (e != sw.engine || Some(m.as_str()) != sw.model.as_deref()) => (Binding { engine: e, model: Some(m), source: "learned" }, sw),
        _ => (sw, sf),
    }
}
/// THE ROUTER'S PICK UNDER A PAIR: the job's side chooses the engine; the side's model, if the pair names one, is the
/// winner (its job cell the fallback); otherwise the engine's own job cell — winner and fallback as measured. The bar
/// shifts as ever. Solo (no pair) = `job_binding_any`. Pure over the file — tested.
pub fn job_binding_paired(job: Job, pair: Option<&Pair>, run_engine: Engine, cost: Option<u8>) -> (Binding, Binding) {
    let Some(p) = pair else { return job_binding_any(job, run_engine, cost) };
    let (engine, named) = if Pair::side(job) == "brain" { (p.brain, p.brain_model.clone()) } else { (p.hands, p.hands_model.clone()) };
    let (w, f) = job_binding(job, engine, cost);
    match named {
        // CODEX NAMES ITS MODELS NOW (2026-09-23): the guard here used to accept only low/medium/high for Codex — a
        // pair that named `gpt-5.6-sol@high` silently fell back to the cell's effort, so a bench of three Codex models
        // measured one model three times. Any named model is honoured; the cost dial still leaves Codex alone.
        Some(m) => {
            let step: i8 = match cost { Some(0) => -1, Some(2) => 1, _ => 0 };
            let m = if step == 0 || engine == Engine::Codex { m } else { shift_model(engine, Some(&m), Tier::Mid, step).unwrap_or(m) };
            let fb = if w.model.as_deref() == Some(m.as_str()) { f } else { w };
            // A FALLBACK THAT IS THE WINNER IS NOT A FALLBACK (found 2026-09-26, after two bench runs died on a dead
            // gateway with no second try). When a pair names the same model the job cell already holds, both sides of
            // this pair came back identical and `run_job` — correctly — declined to retry the thing that had just
            // failed. The pair chooses the ENGINE; within it there is usually another rung, so take the next one down.
            // If the engine has nothing else, the two stay equal and the run fails honestly rather than twice.
            let fb = if fb.engine == engine && fb.model.as_deref() == Some(m.as_str()) {
                match shift_model(engine, Some(&m), Tier::Mid, -1) {
                    Some(other) if other != m => Binding { engine, model: Some(other), source: "pair-fallback" },
                    _ => fb,
                }
            } else { fb };
            (Binding { engine, model: Some(m), source: "pair" }, fb)
        }
        None => (w, f),
    }
}
pub fn set_job_binding(job: &str, engine: &str, winner: Option<&str>, fallback: Option<&str>) -> Result<Value, String> {
    let job = Job::of(job).ok_or("unknown job")?;
    if engine == "any" {
        // the global cell: winner/fallback as "engine/model" or "engine:model"; both empty clears it
        let split = |v: &str| -> Option<(String, String)> { let v = v.trim(); let (e, m) = v.split_once(':').or_else(|| v.split_once(' '))?; engine_of(&json!(e.trim())).map(|e| (e.key().to_string(), m.trim().to_string())) };
        let mut all = chosen_models();
        let jobs = all.as_object_mut().ok_or("bad file")?.entry("jobs".to_string()).or_insert_with(|| json!({}));
        let cell = jobs.as_object_mut().ok_or("bad jobs")?.entry(job.key().to_string()).or_insert_with(|| json!({}));
        match (winner.and_then(split), fallback.and_then(split)) {
            (None, None) => { cell.as_object_mut().map(|o| o.remove("any")); }
            (w, f) => { cell["any"] = json!({ "winner": w.map(|(e, m)| json!({ "engine": e, "model": m })), "fallback": f.map(|(e, m)| json!({ "engine": e, "model": m })) }); }
        }
        let p = models_file(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
        write_atomic(&p, &serde_json::to_string_pretty(&all).unwrap_or_default())?;
        return Ok(all);
    }
    let engine = engine_of(&json!(engine)).ok_or_else(|| format!("engine must be {} | any", Engine::all().iter().map(|e| e.key()).collect::<Vec<_>>().join(" | ")))?;
    let mut all = chosen_models();
    let jobs = all.as_object_mut().ok_or("bad file")?.entry("jobs".to_string()).or_insert_with(|| json!({}));
    let cell = jobs.as_object_mut().ok_or("bad jobs")?.entry(job.key().to_string()).or_insert_with(|| json!({}));
    match (winner.map(str::trim).filter(|m| !m.is_empty()), fallback.map(str::trim).filter(|m| !m.is_empty())) {
        (None, None) => { cell.as_object_mut().map(|o| o.remove(engine.key())); }
        (w, f) => { cell[engine.key()] = json!({ "winner": w, "fallback": f }); }
    }
    let p = models_file(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    write_atomic(&p, &serde_json::to_string_pretty(&all).unwrap_or_default())?;
    Ok(all)
}
/// WHICH JOB IS THIS ASK? Deterministic, no model call (operator 2026-09-22: "add that list as a rule for the
/// Heimdall router selection"). A question is a lookup when it asks for a place, a synthesis when it asks for a reason
/// or an order; a write is S when it names at most one file and stays short, otherwise L. Pure — tested.
pub fn classify_ask(text: &str) -> Job {
    let t = text.trim(); let lower = t.to_ascii_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    let asks = t.contains('?') || ["where", "which", "who", "what", "why", "how", "explain", "describe", "compare", "list", "does", "is", "are", "can", "should"].contains(&first);
    if asks {
        let synth = ["why", "how", "explain", "describe", "compare", "in what order", "in which order", "which order", "walk me", "walk through", "difference", "trade-off", "tradeoff", "should", "if i wanted", "what happens", "what would"];
        return if synth.iter().any(|k| lower.starts_with(k) || lower.contains(&format!(" {k}"))) { Job::Synthesis } else { Job::Lookup };
    }
    // a file is a token with a path separator, or `stem.ext` with a short extension — a sentence's full stop is not one
    let is_file = |w: &str| -> bool {
        if w.starts_with("http") { return false; }
        if w.contains('/') { return true; }
        match w.rsplit_once('.') { Some((stem, ext)) => !stem.is_empty() && (1..=4).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) && stem.chars().last().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false), None => false }
    };
    let files = t.split(|c: char| c.is_whitespace() || c == '`' || c == '"' || c == '\'' || c == ',' || c == '(' || c == ')').filter(|w| is_file(w)).count();
    if files <= 1 && t.chars().count() <= 400 { Job::EditorS } else { Job::EditorL }
}
/// Set (or clear, with `None`) a role's binding.
pub fn set_role_binding(role: &str, engine: Option<&str>, model: Option<&str>) -> Result<Value, String> {
    let role = Role::of(role).ok_or("role must be planner | worker | qa")?;
    let mut all = chosen_models();
    let roles = all.as_object_mut().ok_or("bad file")?.entry("roles".to_string()).or_insert_with(|| json!({}));
    // `auto` (or nothing) hands the role back to the router; anything else is a pin the operator chose
    let auto = engine.is_some_and(|e| e.trim().eq_ignore_ascii_case(ROLE_AUTO));
    match (engine.map(str::trim).filter(|e| !e.is_empty()), model.map(str::trim).filter(|m| !m.is_empty())) {
        (None, None) => { roles.as_object_mut().map(|o| o.remove(role.key())); }
        _ if auto => { roles.as_object_mut().map(|o| o.remove(role.key())); }
        (e, m) => { roles[role.key()] = json!({ "engine": e, "model": m }); }
    }
    let p = models_file(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    write_atomic(&p, &serde_json::to_string_pretty(&all).unwrap_or_default())?;
    Ok(all)
}

/// THE MIGRATION: A PIN ON A SEAT THAT IS GONE BECOMES `auto` (operator 2026-10-01, "Engine config also"; the evidence was last night's Claude-off run: `~/.antiloki/engine-models.json` still named
/// claude-code for the planner and the QA and for Heimdall's own seat, and it only worked because `role_binding` swapped the seat at call time). `ok` answers "may this seat take a call at all" (at start-up: `migration_trusts`, the operator's switches only - NOT installed/signed in, which a launch environment can get wrong, and NOT "has quota right now": a window that empties at four o'clock is not a reason to forget a choice). A role whose engine is such a seat is rewritten to
/// `{"engine": "auto"}`, its model with it (a model name belongs to a seat); `providers.heimdall` goes back to `pane`. Returns one line per change, empty when nothing needed one. Pure over the document - tested.
pub fn migrate_models_doc(all: &mut Value, ok: impl Fn(Engine) -> bool) -> Vec<String> {
    let mut changed = vec![];
    if let Some(roles) = all.get_mut("roles").and_then(Value::as_object_mut) {
        for (name, r) in roles.iter_mut() {
            let Some(engine) = r.get("engine").and_then(Value::as_str).and_then(|e| engine_of(&json!(e))) else { continue };
            if ok(engine) { continue }
            changed.push(format!("roles.{name}: {} → {ROLE_AUTO}", engine.key()));
            *r = json!({ "engine": ROLE_AUTO });
        }
    }
    if let Some(h) = all.get_mut("providers").and_then(|p| p.get_mut("heimdall")) {
        if let Some(engine) = h.as_str().and_then(|e| engine_of(&json!(e))) {
            if !ok(engine) { changed.push(format!("providers.heimdall: {} → pane", engine.key())); *h = json!("pane"); }
        }
    }
    changed
}
/// Write `text` to `path` ATOMICALLY: a temp file beside it, then a rename over it, so a crash (or a second writer) never leaves half a file where the operator's pins were (audit 2026-10-01, F-18:
/// the migration used `fs::write`, which truncates first).
fn write_atomic(path: &std::path::Path, text: &str) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned(); tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| { let _ = std::fs::remove_file(&tmp); format!("{}: {e}", path.display()) })
}
/// Keep a copy of `path` beside it as `<name>.bak-<date>`; when that name is taken (a second migration on the same day), as `.bak-<date>-2`, `-3`... - never over an existing backup, so each
/// run's PRE-STATE survives (audit 2026-10-01, F-18: the second same-day run found `.bak-<date>`, skipped the copy and lost its own pre-state). Returns the backup's path.
fn keep_backup(path: &std::path::Path, date: &str) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    for n in 1..=1000u32 {
        let mut bak = path.as_os_str().to_owned();
        bak.push(if n == 1 { format!(".bak-{date}") } else { format!(".bak-{date}-{n}") });
        let bak = std::path::PathBuf::from(bak);
        // create_new: the name is claimed atomically, so two writers cannot both take it
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&bak) {
            Ok(mut f) => { f.write_all(&bytes).map_err(|e| format!("could not keep {}: {e}", bak.display()))?; return Ok(bak) }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not keep {}: {e}", bak.display())),
        }
    }
    Err(format!("could not keep a backup of {}: a thousand already exist for {date}", path.display()))
}
/// The migration on a FILE: read, migrate, and only when something changed keep a copy beside it (`engine-models.json.bak-<date>`, or `-2`, `-3`... when that day already has one - a backup is never
/// overwritten) and write the new file atomically. A file that is missing, is not an object, or needs nothing is never touched. Returns the lines of what changed.
pub fn migrate_models_file(path: &std::path::Path, ok: impl Fn(Engine) -> bool, date: &str) -> Result<Vec<String>, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(vec![]) };
    let Ok(mut all) = serde_json::from_str::<Value>(&text) else { return Ok(vec![]) };
    if !all.is_object() { return Ok(vec![]) }
    let changed = migrate_models_doc(&mut all, ok);
    if changed.is_empty() { return Ok(changed) }
    keep_backup(path, date)?;
    write_atomic(path, &serde_json::to_string_pretty(&all).unwrap_or_default())?;
    Ok(changed)
}
/// WHAT THE START-UP MIGRATION MAY TRUST (review 2026-10-01): only the operator's own switches - `seats_off` and Zen off. "Not installed" is a lookup on the PATH this process was launched with and
/// "signed out" is a credential file under this process's $HOME; a serve started from a GUI (the desktop sidecar) or a service sees a thinner PATH or HOME than the shell, would find every seat gone
/// and would reset every role and Heimdall's seat to auto/pane, for good. A pin on a seat that is merely not found right now stays: `role_binding` already swaps it at call time, so nothing breaks,
/// and nothing the operator chose is forgotten on a guess. (`engine_usable` is still what the router asks per call.)
pub fn migration_trusts(engine: Engine) -> bool { provider_allowed(engine) }
/// Run at serve start-up, once per process: the operator's own file, the switches as the operator set them.
pub fn migrate_models_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let date = antiloki_rs::clock::now_iso_nodb().chars().take(10).collect::<String>();
        match migrate_models_file(&models_file(), migration_trusts, &date) {
            Ok(c) if !c.is_empty() => eprintln!("antiloki-serve: engine-models.json - {} (a seat the operator switched off is no longer pinned; the old file is kept as engine-models.json.bak-{date}, or -2, -3... when that day already has one)", c.join("; ")),
            Err(e) => eprintln!("antiloki-serve: engine-models.json migration skipped ({e})"),
            _ => {}
        }
    });
}

/// `~/.antiloki/engine-models.json` — the operator's own model per engine × tier, set on the Costs screen. It wins
/// over the env override and the built-in table (2026-09-22: the operator found GPT-5-nano charges on their OpenCode
/// account — antiloki had called `opencode run` with no `--model` and OpenCode used its account default).
pub fn models_file() -> std::path::PathBuf {
    // the unit tests keep their own file (2026-09-22: a role test read the OPERATOR's bindings and failed on them —
    // the same trap the ledger had in 2026-09-19)
    #[cfg(test)]
    // one file PER TEST THREAD, not per process: the roles test writes bindings while the ladder test expects the
    // built-in rungs, and cargo runs them side by side (flaked once on 2026-09-22)
    { return std::env::temp_dir().join(format!("antiloki-models-test-{}-{:?}", std::process::id(), std::thread::current().id())).join("engine-models.json"); }
    #[cfg(not(test))]
    { models_file_real() }
}
#[allow(dead_code)]
fn models_file_real() -> std::path::PathBuf {
    let home = std::env::var("ANTILOKI_HOME").ok().filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into())).join(".antiloki"));
    home.join("engine-models.json")
}
pub fn chosen_models() -> Value { std::fs::read_to_string(models_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({})) }
pub fn set_chosen_model(engine: &str, tier: &str, model: Option<&str>) -> Result<Value, String> {
    let mut all = chosen_models();
    let e = all.as_object_mut().ok_or("bad file")?.entry(engine.to_string()).or_insert_with(|| json!({}));
    match model { Some(m) if !m.trim().is_empty() => { e[tier] = json!(m.trim()); } _ => { e.as_object_mut().map(|o| o.remove(tier)); } }
    let p = models_file(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    write_atomic(&p, &serde_json::to_string_pretty(&all).unwrap_or_default())?;
    Ok(all)
}
/// The model for a call. The operator's file wins, then `ANTILOKI_TIER_MODEL_<ENGINE>_<TIER>`, then the built-in
/// table. OPENCODE IS NEVER LEFT WITHOUT ONE: its catalog is large and its account default is whatever the provider
/// picked (a GPT-5-nano, on the operator's account, 2026-09-22) — an absent tier falls back to the mid-tier model.
pub fn tier_model(engine: Engine, tier: Tier) -> Option<String> {
    if let Some(m) = chosen_models().get(engine.key()).and_then(|e| e.get(tier.key())).and_then(Value::as_str) {
        let m = m.trim(); if !m.is_empty() { return Some(m.to_string()); }
    }
    tier_model_builtin(engine, tier)
}
/// The model a call gets when no tier was asked for — only OpenCode needs one (the others have a single account model).
pub fn model_for_untiered(engine: Engine) -> Option<String> { if engine == Engine::Opencode { tier_model(engine, Tier::Mid) } else { None } }
fn tier_model_builtin(engine: Engine, tier: Tier) -> Option<String> {
    let env_key = format!("ANTILOKI_TIER_MODEL_{}_{}", engine.key().to_ascii_uppercase().replace('-', "_"), tier.key().to_ascii_uppercase());
    if let Ok(v) = std::env::var(&env_key) { let v = v.trim().to_string(); return if v.is_empty() || v == "default" { None } else { Some(v) }; }
    match (engine, tier) {
        // the bench of 2026-09-21 (8 sub-agents on ministore): low and mid were BOTH sonnet, so a "low" route bought
        // nothing — haiku is 3× cheaper per token and writes a small module from a complete spec as well
        (Engine::ClaudeCode, Tier::Low) => Some("haiku".into()),
        (Engine::ClaudeCode, Tier::Mid) => Some("sonnet".into()),
        // NEVER OPUS (operator 2026-11-13, said three times): the strongest tier is Sonnet 5.5 for Claude Code. Set ANTILOKI_TIER_MODEL_CLAUDE_CODE_HIGH to choose another.
        (Engine::ClaudeCode, Tier::High) => Some("sonnet".into()),
        // codex: the ChatGPT subscription refuses `gpt-5-mini` ("unsupported for Codex with this account",
        // found by a real run 2026-09-19) — the tiers are REASONING EFFORT on the account's default model,
        // never a different model
        // opencode: the Go/Zen catalog table lives in core (the pane's opencode.json pins the same cells)
        (Engine::Opencode, t) => antiloki_rs::cockpit_cockpit_pane_add::opencode_go_model(t.key()),
        // GROK'S TIERS ARE REASONING EFFORT, like Codex's — one model, three efforts (`--reasoning-effort`). A bare
        // effort here is turned into that flag by `cli_command`, never into `-m`.
        (Engine::Grok, Tier::Low) => Some("low".into()),
        (Engine::Grok, Tier::High) => Some("high".into()),
        // CURSOR IS USED ONLY FOR WHAT IT ADDS (operator 2026-09-27: "let's keep using the cheaper part of cursor,
        // like grok and maybe composer"). `cursor-agent --list-models` on the Pro plan also serves
        // claude-opus-5-thinking-high, claude-sonnet-5-thinking-high and the gpt-5.x/codex family — but reaching
        // those THROUGH Cursor spends a Cursor quota to get a model the operator already holds on Claude Max or a
        // ChatGPT plan. The additive models are Cursor's own Composer and its Grok, so those are the ladder; an
        // operator can still pin any id by name.
        // CURSOR PRO HAS TWO POOLS AND ONLY ONE OF THEM EMPTIES (researched 2026-09-27). Cursor's OWN models —
        // Composer 2.5 and the `cursor-grok-*` builds of Grok 4.5/4.6 — draw on a generous pool whose limits Cursor
        // permanently doubled, and Composer 2.5 carries NO token rate on any plan. Everything else (Claude, GPT,
        // Gemini, and Grok 4.7 — the one Grok WITHOUT the `cursor-` prefix) is billed at API rates against the
        // $20/month third-party allowance, the pool that runs out. Composer at $0.50/M input against Claude Fable at
        // $10/M is a twentyfold difference on the same task.
        // So every rung stays inside Cursor's own pool, and `-fast` variants are avoided: priority compute costs more
        // for the same answer.
        (Engine::Cursor, Tier::Low) => Some("composer-2.5".into()),
        (Engine::Cursor, Tier::Mid) => Some("cursor-grok-4.6-medium".into()),
        (Engine::Cursor, Tier::High) => Some("cursor-grok-4.6-high".into()),
        // GEMINI'S RUNGS, READ FROM THE SEAT (2026-09-27). `agy models` on a signed-in account lists
        // gemini-3.8/3.7/3.6-flash at low/medium/high and gemini-3.1-pro at low/high, so the ladder is the newest
        // flash for the cheap rungs and Pro at the top. This matters more than it looks: the first Gemini bench ran
        // on the ACCOUNT DEFAULT and took 262 s against Grok's 30 s, which measured the default's effort rather than
        // Gemini's fitness for small work.
        (Engine::Gemini, Tier::Low) => Some("gemini-3.8-flash-low".into()),
        (Engine::Gemini, Tier::Mid) => Some("gemini-3.8-flash-medium".into()),
        (Engine::Gemini, Tier::High) => Some("gemini-3.1-pro-high".into()),
        _ => None, // codex (all tiers): the account's default model
    }
}

/// PUBLIC LIST PRICES per engine × tier — what the subscription pays for, in API dollars ($in, $out per
/// Mtok) — the yardstick Heimdall's savings are measured with. OpenCode's Go models are DeepSeek's
/// public prices (approximate). Pure — tested.
pub fn list_price(engine: Engine, tier: Tier) -> (&'static str, f64, f64) {
    match (engine, tier) {
        // opus at $5/$25 since 4.5 (bench 2026-09-21: the CLI's own total_cost_usd for a raw opus run matched these, not
        // the $15/$75 of 4.1 the table carried — which had every "saved vs opus" figure inflated 3×)
        // a PRICE REFERENCE only (the savings figures compare against a frontier baseline); no call is made on it: the High tier's own model is Sonnet (operator 2026-11-13: never Opus)
        (Engine::ClaudeCode, Tier::High) => ("opus", 5.0, 25.0),
        (Engine::ClaudeCode, Tier::Low) => ("haiku", 1.0, 5.0),
        (Engine::ClaudeCode, _) => ("sonnet", 3.0, 15.0),
        (Engine::Codex, Tier::High) => ("gpt-5 · high effort", 1.25, 10.0),
        (Engine::Codex, Tier::Low) => ("gpt-5 · low effort", 1.25, 10.0),
        (Engine::Codex, _) => ("gpt-5", 1.25, 10.0),
        (Engine::Opencode, Tier::Low) => ("deepseek-v4-flash", 0.14, 0.28),
        (Engine::Opencode, _) => ("deepseek-v4-pro", 0.55, 2.19),
        // APPROXIMATE, AND SAID SO. These three are subscription seats: the per-token figure is the yardstick a
        // saving is measured against, never a bill the operator receives. Gemini Flash at $0.14/$0.28 is the
        // cheapest rung any engine here offers.
        (Engine::Gemini, Tier::Low) => ("gemini flash", 0.14, 0.28),
        (Engine::Gemini, _) => ("gemini pro", 1.25, 10.0),
        (Engine::Grok, Tier::Low) => ("grok code fast", 0.20, 1.50),
        (Engine::Grok, _) => ("grok", 3.0, 15.0),
        (Engine::Cursor, Tier::Low) => ("composer 2.5", 0.50, 1.50),
        (Engine::Cursor, _) => ("grok 4.6", 3.0, 15.0),
    }
}
/// OPENCODE NAMES A MODEL `provider/model` (2026-09-22: a bare `deepseek-v4-flash` on the command line gets
/// `ProviderModelNotFoundError: Model not found: deepseek-v4-flash/.` — reported by the CLI as an opaque
/// "UnknownError · Unexpected server error", which read as an outage for an hour). The operator may type the bare
/// name on the settings screen or in engine-models.json; the wire always gets the catalogue's form. Pure — tested.
pub fn opencode_model_id(model: &str) -> String {
    let m = model.trim();
    if m.contains('/') { m.to_string() } else { format!("opencode/{m}") }
}
/// The command a parent terminal types to move itself to a tier's model — the router's advice when
/// it decides "one job, no delegation" (2026-09-20: "if no need for subagents will also decide which
/// model will use for the task"). Pure.
pub fn model_switch(engine: Engine, tier: Tier) -> &'static str {
    match (engine, tier) {
        (Engine::ClaudeCode, Tier::Low) => "/model haiku",
        (Engine::ClaudeCode, _) => "/model sonnet",
        (Engine::Codex, Tier::High) => "/model · reasoning effort high",
        (Engine::Codex, Tier::Low) => "/model · reasoning effort low",
        (Engine::Codex, _) => "/model · reasoning effort medium",
        (Engine::Opencode, Tier::Low) => "/models → deepseek-v4-flash",
        (Engine::Opencode, _) => "/models → deepseek-v4-pro",
        (Engine::Gemini, Tier::Low) => "/model → gemini flash",
        (Engine::Gemini, _) => "/model → gemini pro",
        (Engine::Grok, Tier::High) => "/model · reasoning effort high",
        (Engine::Grok, Tier::Low) => "/model · reasoning effort low",
        (Engine::Grok, _) => "/model · reasoning effort medium",
        (Engine::Cursor, Tier::Low) => "/model → composer-2.5",
        (Engine::Cursor, _) => "/model → cursor-grok-4.6",
    }
}

/// The provider's own name — never the bare engine key. Pure.
pub fn provider_name(e: Engine) -> &'static str {
    match e {
        Engine::ClaudeCode => "Claude", Engine::Codex => "Codex", Engine::Opencode => "OpenCode",
        Engine::Gemini => "Antigravity", Engine::Grok => "Grok", Engine::Cursor => "Cursor",
    }
}
/// A model written the way its maker writes it — never the raw id; "" when the job decides it. Pure.
/// THE ONE SPELLING OF A MODEL, for the RECORD (audit 2026-09-28). A model reaches the record under two names: the
/// alias the binding asked for (`sonnet`) and the full id the CLI reports back (`claude-sonnet-5`). Both are written
/// as `merges::key` sides, so one model's spend was split in two — `claude-code:sonnet` and
/// `claude-code:claude-sonnet-5` are the same Sonnet, and neither total was the real one. That is the operator's
/// own complaint in its narrowest form: "the sonnet benchmark is the same and should be measured as one, but the
/// origins must be explicitly so we know whats is spending our money". The ORIGIN — the provider — is untouched;
/// only the model half is folded.
///
/// CLAUDE CODE ONLY, DELIBERATELY. Its aliases are unambiguous: `--model sonnet` resolves to the current Sonnet, so
/// `claude-sonnet-5` and `sonnet` are provably one model. Codex is NOT safe to fold the same way — `gpt-5.6-terra`,
/// `gpt-5.6-luna` and `gpt-5.6-sol` are different models that all appear at `@medium`, and collapsing them to
/// `medium` would merge evidence for three models into one row, which is worse than the split it fixes. Gemini's
/// `default` likewise names whatever the seat chose, which is not knowable from the string. Those stay as they are
/// until there is a mapping that cannot be wrong. Pure — tested.
pub fn model_canon(engine: Engine, model: &str) -> String {
    let m = model.trim();
    if engine != Engine::ClaudeCode || !m.starts_with("claude-") { return m.to_string() }
    // `claude-opus-5[1m]` is Opus with a wider window — the same model, and the window is not what a row identifies
    let base = m.split('[').next().unwrap_or(m);
    for family in ["opus", "sonnet", "haiku"] {
        if base.split('-').any(|w| w == family) { return family.to_string() }
    }
    m.to_string()
}

pub fn model_name(model: Option<&str>) -> String {
    let m = model.unwrap_or("").trim().trim_start_matches("opencode/");
    if m.is_empty() || m == "default" { return String::new(); }
    let (base, effort) = m.split_once('@').map(|(a, b)| (a, Some(b))).unwrap_or((m, None));
    let pretty = match base {
        "deepseek-v4-flash" => "DeepSeek v4 Flash".to_string(),
        "deepseek-v4-pro" => "DeepSeek v4 Pro".to_string(),
        "qwen3.6-plus" => "Qwen 3.6 Plus".to_string(),
        "kimi-k2.7-code" => "Kimi K2.7 Code".to_string(),
        "opus" | "sonnet" | "haiku" => { let mut c = base.chars(); c.next().map(|f| f.to_uppercase().to_string() + c.as_str()).unwrap_or_default() }
        // the full ids the CLIs actually report — `claude-sonnet-5`, `claude-opus-5[1m]` — read as the family and
        // its version, never as the raw string (the provider is printed by `side_name`, so "claude" is dropped)
        b if b.starts_with("claude-") => {
            let cap = |w: &str| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().to_string() + c.as_str()).unwrap_or_default() };
            b.trim_start_matches("claude-").split('-')
             .map(|w| if w.chars().next().is_some_and(|c| c.is_ascii_digit()) { w.to_string() } else { cap(w) })
             .collect::<Vec<_>>().join(" ")
        }
        b if b.starts_with("glm") => format!("GLM {}", b.trim_start_matches("glm-")),
        // THE SUBSCRIPTION SEATS' OWN NAMES (2026-09-27). Without these a merge label read
        // "Cursor cursor-grok-4.6-high × Cursor composer-2.5", which is a model id, not a name.
        // Cursor's own Grok builds carry a `cursor-` prefix that marks the POOL, not the model, so it is dropped here.
        b if b.starts_with("composer") => format!("Composer {}", b.trim_start_matches("composer-")),
        // CURSOR'S GROK AND xAI'S GROK ARE NOT THE SAME SEAT (operator 2026-09-28: "there is grok and there is
        // cursor-grok — make the difference between them visible"). The `cursor-` prefix marks the POOL, and the
        // pool is the whole point: `cursor-grok-4.6` draws on Cursor's own allowance at no token rate, while
        // `grok-code-fast-1` through the xAI CLI is a subscription of its own. `side_name` prints the provider,
        // so the model half never repeats the vendor — the two read as "Cursor · Grok 4.6 · medium" and
        // "Grok · Code Fast 1", which is the distinction the operator needs in front of them.
        b if b.starts_with("cursor-grok-") => {
            let rest = b.trim_start_matches("cursor-grok-");
            match rest.split_once('-') { Some((v, ef)) => format!("Grok {v} · {ef}"), None => format!("Grok {rest}") }
        }
        b if b.starts_with("grok-") => {
            let cap = |w: &str| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().to_string() + c.as_str()).unwrap_or_default() };
            b.trim_start_matches("grok-").split('-')
             .map(|w| if w.chars().next().is_some_and(|c| c.is_ascii_digit()) { w.to_string() } else { cap(w) })
             .collect::<Vec<_>>().join(" ")
        }
        // the provider is printed by `side_name`, so the model name never repeats it — "Gemini 3.8 Flash", not
        // "Gemini · Gemini 3.8 Flash" (the same reason `deepseek-v4-flash` renders without "OpenCode")
        b if b.starts_with("gemini-") => {
            let parts: Vec<&str> = b.trim_start_matches("gemini-").split('-').collect();
            let cap = |w: &str| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().to_string() + c.as_str()).unwrap_or_default() };
            parts.iter().map(|w| if w.chars().next().is_some_and(|c| c.is_ascii_digit()) { w.to_string() } else { cap(w) }).collect::<Vec<_>>().join(" ")
        }
        b => b.to_string(),
    };
    match effort { Some(e) => format!("{pretty} · {e}"), None => pretty }
}
/// One side of a merge, always provider AND model. Pure — tested.
pub fn side_name(engine: Engine, model: Option<&str>) -> String {
    if engine == Engine::Codex {
        // Codex is a provider: the binding may carry a model, an effort, or both — the name says which it has
        let (m, ef) = codex_model_effort(model.unwrap_or(""));
        let name = m.map(|x| model_name(Some(&x))).unwrap_or_default();
        return match (name.is_empty(), ef) {
            (false, Some(e)) => format!("Codex {name} · {e}"),
            (false, None) => format!("Codex {name}"),
            (true, Some(e)) => format!("Codex · the account's model · {e}"),
            (true, None) => "Codex · the job's model".to_string(),
        };
    }
    // THE ORIGIN IS ALWAYS SAID (operator 2026-09-28: "always say the provider with the model … Antropic-sonnet
    // is different from cursor-sonnet and also from opencode-sonnet … so we know what is spending our money").
    //
    // The same model reached through a different seat is a different BILL: Composer on Cursor's own pool carries
    // no token rate, the identical Grok build reached through the xAI CLI is a second subscription, and a Claude
    // model reached through Cursor bills the $20 third-party allowance rather than the Claude seat. A label that
    // says only the model hides the one fact the operator is reading it for.
    let m = model_name(model);
    if m.is_empty() { format!("{} · the job's model", provider_name(engine)) } else { format!("{} · {m}", provider_name(engine)) }
}
/// CODEX: the model and the effort out of one binding string. `"gpt-5.6-sol@high"` → both; `"high"` → the effort only
/// (the account's own model); `"gpt-5.6-terra"` → the model only. An unknown word is treated as a model name, because
/// that is what a new Codex release looks like. Pure — tested.
pub fn codex_model_effort(m: &str) -> (Option<String>, Option<String>) {
    let m = m.trim();
    let efforts = ["low", "medium", "high"];
    if let Some((name, ef)) = m.split_once('@').or_else(|| m.split_once(':')) {
        let (name, ef) = (name.trim(), ef.trim());
        return (if name.is_empty() { None } else { Some(name.to_string()) },
                if efforts.contains(&ef) { Some(ef.to_string()) } else { None });
    }
    if efforts.contains(&m) { (None, Some(m.to_string())) } else if m.is_empty() { (None, None) } else { (Some(m.to_string()), None) }
}
/// The argv for one engine — the prompt goes on stdin; nothing user-typed lands in argv. Claude and
/// Codex stream JSON events so the audit can list every tool the CLI ran for the answer.
pub fn cli_command(engine: Engine, system: &str, opts: &RunOpts) -> (&'static str, Vec<String>) {
    match engine {
        Engine::ClaudeCode => {
            let turns = opts.max_turns.unwrap_or(if opts.write { 30 } else { 6 });
            let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose", "--max-turns", &turns.to_string(), "--allowedTools", &{ let mut t = String::from(if opts.write { "Read,Grep,Glob,Edit,Write,MultiEdit,Bash" } else { "Read,Grep,Glob" }); for x in &opts.extra_tools { if tool_name_ok(x) { t.push(','); t.push_str(x); } } t }].iter().map(|s| s.to_string()).collect();
            if !system.is_empty() { args.push("--append-system-prompt".into()); args.push(system.into()); }
            // NEVER OPUS (operator 2026-11-13, said four times): whatever route, tier, role table or hand-typed id asked for it, a Claude Code call is made on Sonnet instead
            if let Some(m) = opts.model.clone().or_else(|| opts.tier.and_then(|t| tier_model(engine, t))) { args.push("--model".into()); args.push(if m.to_ascii_lowercase().contains("opus") { "sonnet".to_string() } else { m }); }
            if let Some(f) = &opts.mcp_config { args.push("--mcp-config".into()); args.push(f.clone()); if opts.strict_mcp { args.push("--strict-mcp-config".into()); } }
            if let Some(b) = opts.max_budget_usd.filter(|b| *b > 0.0) { args.push("--max-budget-usd".into()); args.push(format!("{b:.2}")); }
            if let Some(f) = &opts.settings { args.push("--settings".into()); args.push(f.clone()); }
            if !opts.disallowed.is_empty() { args.push("--disallowedTools".into()); args.push(opts.disallowed.iter().filter(|x| tool_name_ok(x)).cloned().collect::<Vec<_>>().join(",")); }
            ("claude", args)
        }
        Engine::Codex => {
            // behind antiloki's own wall Codex's sandbox cannot start inside another one, and would add nothing
            let mut args: Vec<String> = if opts.wall.is_some() { ["exec", "--json", "--skip-git-repo-check", "--dangerously-bypass-approvals-and-sandbox", "-c", "mcp_servers={}"].iter().map(|s| s.to_string()).collect() } else { ["exec", "--json", "--skip-git-repo-check", "--sandbox", if opts.write { "workspace-write" } else { "read-only" }].iter().map(|s| s.to_string()).collect() };
            // CODEX IS A PROVIDER, NOT A MODEL (operator 2026-09-23). It serves named models — on a Plus plan
            // gpt-5.6-terra · -sol · -luna — and each runs at a reasoning effort. `codex_model_effort` splits the two
            // out of one string: "gpt-5.6-sol@high" is a model and an effort, a bare effort is the account's default
            // model at that effort, a bare name is that model at the config's effort.
            if let Some(m) = opts.model.clone().or_else(|| opts.tier.and_then(|t| tier_model(engine, t))) {
                let (model, effort) = codex_model_effort(&m);
                if let Some(mm) = model { args.push("-m".into()); args.push(mm); }
                if let Some(ef) = effort { args.push("-c".into()); args.push(format!("model_reasoning_effort=\"{ef}\"")); }
            }
            match opts.tier { Some(Tier::High) => { args.push("-c".into()); args.push("model_reasoning_effort=\"high\"".into()); } Some(Tier::Low) => { args.push("-c".into()); args.push("model_reasoning_effort=\"low\"".into()); } _ => {} }
            if let Some(f) = &opts.last_message_file { args.push("--output-last-message".into()); args.push(f.clone()); }
            for img in &opts.images { args.push("-i".into()); args.push(img.clone()); }
            args.push("-".into());
            ("codex", args)
        }
        // `--format json` (2026-09-22): the event stream carries every step's tokens AND OpenCode's own cost for its catalog —
        // the plain format had neither, so a flash worker's work was priced as nothing
        Engine::Opencode => { let mut args = vec!["run".to_string(), "--format".to_string(), "json".to_string()];
            // BEHIND THE WALL OPENCODE RUNS ITS OWN SERVER: its usual background service is a process outside the sandbox, which would do the work with nothing holding it (found 2026-11-27)
            if opts.wall.is_some() { args.push("--standalone".to_string()); if opts.write { args.push("--auto".to_string()); } }
            if let Some(m) = opts.model.clone().or_else(|| opts.tier.and_then(|t| tier_model(engine, t))).or_else(|| model_for_untiered(engine)) { args.push("--model".into()); args.push(opencode_model_id(&m)); } ("opencode", args) }
        // GEMINI TAKES THE PROMPT ON STDIN (probed 2026-09-27): `-p` is documented as "appended to input on stdin (if
        // any)", and `-o json … -p ""` with the prompt piped in parses and reaches auth. `--approval-mode plan` is
        // the CLI's own read-only mode; `yolo` is its write mode. It has no system-prompt flag, so the system text is
        // folded into the prompt by `run_cli_inner`.
        Engine::Gemini => {
            let model = opts.model.clone().or_else(|| opts.tier.and_then(|t| tier_model(engine, t)));
            let prompt_for_argv = opts.prompt_argv.as_deref().unwrap_or("");
            match engine.bin_present().unwrap_or("agy") {
                // ANTIGRAVITY CLI, the live subscription path (its own headless docs, read 2026-09-27): one prompt,
                // one exit, a JSON envelope of {response, usage, status}. `--dangerously-skip-permissions` is its
                // only unattended approval mode — headless has no prompt, so a tool needing approval is soft-denied
                // and the run would otherwise finish having quietly done nothing.
                "agy" => {
                    // stream-json, not json (2026-10-06): the single envelope has no tool calls, so an Antigravity task showed 0 tools. The stream has one `step_update` per step - a tool step is
                    // `{"step_type":"tool","tool_name":…,"tool_info":{"parameters":{…}}}` - and ends in the same fields the single envelope held, under `result` (`parse_agy_stream`).
                    let mut args: Vec<String> = vec!["--output-format".into(), "stream-json".into()];
                    // A SOFT-DENY IS NOT A READ-ONLY RUN (probed 2026-09-27). Antigravity auto-denies any tool needing
                    // a permission headless cannot prompt for, and then returns `status: SUCCESS` with an EMPTY response
                    // and exit 0 — a silent no-answer wearing the shape of a success. A read-only judge or verifier hit
                    // this on every call: it wanted to run a command to check a claim and got nothing back.
                    // `--mode plan` is what keeps a read-only run read-only (it cannot edit); the skip flag only
                    // removes a prompt that headless mode is structurally unable to answer. Both are needed.
                    if opts.write { args.push("--dangerously-skip-permissions".into()); }
                    else { args.push("--mode".into()); args.push("plan".into()); args.push("--dangerously-skip-permissions".into()); }
                    if let Some(m) = model { args.push("--model".into()); args.push(m); }
                    // ANTIGRAVITY DOES NOT READ THE PROMPT FROM STDIN (found by a real run, 2026-09-27). This passed
                    // `-p ""` and piped the prompt, which is what the LEGACY `gemini` CLI does — its own help says `-p`
                    // is "appended to input on stdin (if any)". `agy` is a different program and rejects it outright:
                    // `Error: empty prompt. Usage: agy --print "your prompt here"`, so every Gemini task failed at the
                    // invocation. Its only stdin path is `--input-format stream-json`, which also forces
                    // `--output-format stream-json` and a different envelope; single-turn takes the prompt in argv.
                    //
                    // This is the one engine here whose prompt lands in argv. There is no quoting risk — `proc::tokio_cmd`
                    // passes argv directly, never through a shell — but the prompt IS visible in a process listing, which
                    // is the reason the other engines avoid it. Switching to stream-json would remove that; it is a
                    // bigger change than making Gemini work at all, so it is noted rather than done.
                    args.push("-p".into()); args.push(prompt_for_argv.to_string());
                    ("agy", args)
                }
                // the legacy CLI, kept for an operator who holds a paid API key: `-p` is documented as appended to
                // stdin, `--approval-mode plan` is its read-only mode (both probed 2026-09-27)
                _ => {
                    let mut args: Vec<String> = vec!["-o".into(), "json".into(), "--approval-mode".into(),
                                                     if opts.write { "yolo".into() } else { "plan".into() }];
                    if let Some(m) = model { args.push("-m".into()); args.push(m); }
                    args.push("-p".into()); args.push(String::new());
                    ("gemini", args)
                }
            }
        }
        // GROK CANNOT READ A PIPED PROMPT (probed 2026-09-27: bare stdin dies with `No such device or address (os
        // error 6)` — it opens a tty). `--prompt-file` is its single-turn form, and `run_cli_inner` writes the file,
        // which also keeps the prompt out of argv. Its `--permission-mode` values are the ones the CLI lists:
        // `plan` is read-only, `bypassPermissions` is the headless write mode.
        Engine::Grok => {
            let mut args: Vec<String> = vec!["--output-format".into(), "json".into(), "--permission-mode".into(),
                                             if opts.write { "bypassPermissions".into() } else { "plan".into() }];
            args.push("--max-turns".into()); args.push(opts.max_turns.unwrap_or(if opts.write { 30 } else { 6 }).to_string());
            if !system.is_empty() { args.push("--system-prompt-override".into()); args.push(system.into()); }
            // one model string, split like Codex's: a bare effort is the account model at that effort
            if let Some(m) = opts.model.clone().or_else(|| opts.tier.and_then(|t| tier_model(engine, t))) {
                let (model, effort) = codex_model_effort(&m);
                if let Some(mm) = model { args.push("-m".into()); args.push(mm); }
                if let Some(ef) = effort { args.push("--reasoning-effort".into()); args.push(ef); }
            }
            if let Some(f) = &opts.prompt_file { args.push("--prompt-file".into()); args.push(f.clone()); }
            ("grok", args)
        }
        // CURSOR IS NOT GROK (operator asked, 2026-09-27): its own model is Composer, and it also proxies
        // Claude/GPT/Gemini — a separate fifth seat. `cursor-agent` is NOT installed on this machine, so this argv
        // follows the documented `-p`/`--output-format` shape and is UNVERIFIED until the binary exists.
        // CURSOR, VERIFIED BY A REAL CALL (2026-09-27): `-p --output-format json` answers
        // {"type":"result","result":…,"usage":{inputTokens,outputTokens}}. It refuses to run in a directory it does
        // not trust ("Workspace Trust Required"), which in a piped subprocess is a silent stall — `--trust` is what
        // makes a headless run possible at all, so it is passed on every run, write or not.
        Engine::Cursor => {
            // stream-json, not json (2026-10-06): the single `json` envelope carries no tool calls, so a Cursor task showed 0 tools in the timeline and the audit. The stream has one `tool_call` event per call
            // (probed live: `{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":…}}}}`) and the same `result` line last - `parse_cursor_stream` reads both.
            let mut args: Vec<String> = vec!["-p".into(), "--output-format".into(), "stream-json".into(), "--trust".into()];
            if opts.write { args.push("--force".into()); } else { args.push("--mode".into()); args.push("plan".into()); }
            if let Some(m) = opts.model.clone().or_else(|| opts.tier.and_then(|t| tier_model(engine, t))) { args.push("--model".into()); args.push(m); }
            ("cursor-agent", args)
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CliTool { pub name: String, pub target: String }
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct CliParsed { pub content: String, pub tokens_in: Option<i64>, pub tokens_out: Option<i64>, pub cost: Option<f64>, pub tools: Vec<CliTool>, pub error: Option<String> }

fn json_lines(s: &str) -> Vec<Value> { s.lines().map(str::trim).filter(|l| l.starts_with('{')).filter_map(|l| serde_json::from_str(l).ok()).collect() }
fn tool_target(input: Option<&Value>) -> String {
    let Some(o) = input.and_then(Value::as_object) else { return String::new() };
    ["file_path", "filePath", "path", "pattern", "command", "query", "url"].iter().filter_map(|k| o.get(*k)).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).next().unwrap_or_default().chars().take(200).collect()
}
fn num(v: Option<&Value>) -> i64 { v.and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))).unwrap_or(0) }

/// What each CLI printed → the answer text, what it reported about tokens/cost, and the tools it ran.
pub fn parse_cli_output(engine: Engine, raw_stdout: &str, last_message: Option<&str>) -> CliParsed {
    // a version-manager shim (mise) prints its banner before the CLI speaks — never part of the answer
    let stdout: String = raw_stdout.lines().filter(|l| !(l.starts_with("mise ") && l.contains(" tools:"))).collect::<Vec<_>>().join("\n");
    match engine {
        Engine::ClaudeCode => {
            let evs = json_lines(&stdout);
            let mut tools = Vec::new();
            for e in &evs {
                if e.get("type").and_then(Value::as_str) == Some("assistant") {
                    for c in e.get("message").and_then(|m| m.get("content")).and_then(Value::as_array).cloned().unwrap_or_default() {
                        if c.get("type").and_then(Value::as_str) == Some("tool_use") { tools.push(CliTool { name: c.get("name").and_then(Value::as_str).unwrap_or("tool").into(), target: tool_target(c.get("input")) }); }
                    }
                }
            }
            // PRICED tokens (bench 2026-09-21): a cache read costs a tenth of an input token and a cache write 1.25× —
            // the raw sum priced everything at the input rate, 5–8× what the API charges, and made the ledger's
            // dollars incomparable with the CLI's own `total_cost_usd`. The same yardstick as agent.rs::turn_tokens.
            let usage_of = |u: Option<&Value>| -> (Option<i64>, Option<i64>) {
                let u = u.cloned().unwrap_or(json!({}));
                let tin = (num(u.get("input_tokens")) as f64 + 0.1 * num(u.get("cache_read_input_tokens")) as f64 + 1.25 * num(u.get("cache_creation_input_tokens")) as f64).round() as i64;
                (if tin > 0 { Some(tin) } else { None }, Some(num(u.get("output_tokens"))).filter(|n| *n > 0))
            };
            if let Some(r) = evs.iter().find(|e| e.get("type").and_then(Value::as_str) == Some("result")) {
                let (tokens_in, tokens_out) = usage_of(r.get("usage"));
                let sub = r.get("subtype").and_then(Value::as_str).unwrap_or("");
                // the CLI's own verdict (error_max_turns, error_during_execution …) is the error, not "no text"
                let error = if sub.starts_with("error") {
                    let detail = if r.get("is_error").and_then(Value::as_bool).unwrap_or(false) { r.get("result").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| format!(": {}", s.chars().take(300).collect::<String>())).unwrap_or_default() } else { String::new() };
                    Some(format!("{}{detail}", sub.trim_start_matches("error_").replace('_', " ")))
                } else { None };
                return CliParsed { content: if error.is_some() { String::new() } else { r.get("result").and_then(Value::as_str).unwrap_or("").into() }, tokens_in, tokens_out, cost: r.get("total_cost_usd").and_then(Value::as_f64), tools, error };
            }
            // `--output-format json` (one object) still parses
            let start = stdout.find('{').unwrap_or(0);
            if let Ok(j) = serde_json::from_str::<Value>(&stdout[start..]) {
                let (tokens_in, tokens_out) = usage_of(j.get("usage"));
                return CliParsed { content: j.get("result").and_then(Value::as_str).unwrap_or("").into(), tokens_in, tokens_out, cost: j.get("total_cost_usd").and_then(Value::as_f64), tools, error: None };
            }
            CliParsed { content: stdout.trim().into(), tools, ..Default::default() }
        }
        Engine::Codex => {
            let evs = json_lines(&stdout);
            let mut tools = Vec::new(); let mut last = String::new(); let mut tin = None; let mut tout = None; let mut err: Option<String> = None;
            for e in &evs {
                let it = e.get("item").cloned().unwrap_or(Value::Null);
                let ty = e.get("type").and_then(Value::as_str).unwrap_or("");
                // `{"type":"error","message":"You've hit your usage limit…"}` (2026-09-20): the stream ends with no
                // message — the error is the answer, never the raw events
                if ty == "error" || ty == "turn.failed" { if let Some(m) = e.get("message").or_else(|| e.get("error").and_then(|x| x.get("message"))).and_then(Value::as_str) { err = Some(m.trim().to_string()); } }
                if ty == "item.completed" && it.get("type").and_then(Value::as_str) == Some("command_execution") {
                    let cmd = it.get("command").and_then(Value::as_str).unwrap_or("").trim_start_matches("/usr/bin/bash -lc").trim();
                    let cmd = cmd.strip_prefix('\'').and_then(|c| c.strip_suffix('\'')).unwrap_or(cmd);
                    tools.push(CliTool { name: "shell".into(), target: cmd.chars().take(200).collect() });
                }
                if ty == "item.completed" && it.get("type").and_then(Value::as_str) == Some("agent_message") { if let Some(t) = it.get("text").and_then(Value::as_str) { last = t.to_string(); } }
                if ty == "turn.completed" { if let Some(u) = e.get("usage") { tin = Some(num(u.get("input_tokens"))).filter(|n| *n > 0); tout = Some(num(u.get("output_tokens"))).filter(|n| *n > 0); } }
            }
            let legacy = regex::Regex::new(r"tokens used\s*\n\s*([\d,]+)").unwrap().captures(&stdout).and_then(|c| c[1].replace(',', "").parse::<i64>().ok());
            let content = last_message.map(str::trim).filter(|s| !s.is_empty()).map(String::from)
                .or_else(|| Some(last.trim().to_string()).filter(|s| !s.is_empty()))
                // the legacy text format only — a JSONL stream without a message is NOT an answer
                .or_else(|| if evs.is_empty() { regex::Regex::new(r"\n(?:codex|assistant)\n").unwrap().split(&stdout).last().and_then(|s| s.split("\ntokens used").next()).map(|s| s.trim().to_string()) } else { None })
                .unwrap_or_default();
            CliParsed { content, tokens_in: tin, tokens_out: tout.or(legacy), cost: None, tools, error: err }
        }
        Engine::Opencode => {
            // `--format json`: one event per line — `text` parts are the answer, `step_finish` carries tokens
            // (input/output/cache) and OpenCode's cost for the step, `tool_use` names the tool and its target
            let evs = json_lines(&stdout);
            if !evs.is_empty() {
                let mut content = String::new(); let (mut tin, mut tout, mut cost) = (0f64, 0f64, 0f64); let mut tools = vec![];
                for e in &evs {
                    let p = e.get("part").cloned().unwrap_or(Value::Null);
                    match p.get("type").and_then(Value::as_str).unwrap_or("") {
                        "text" => content.push_str(p.get("text").and_then(Value::as_str).unwrap_or("")),
                        "step-finish" => {
                            let t = p.get("tokens").cloned().unwrap_or(json!({}));
                            let cache = t.get("cache").cloned().unwrap_or(json!({}));
                            // priced like Claude's: a cache read a tenth, a cache write 1.25×
                            tin += num(t.get("input")) as f64 + 0.1 * num(cache.get("read")) as f64 + 1.25 * num(cache.get("write")) as f64;
                            tout += num(t.get("output")) as f64 + num(t.get("reasoning")) as f64;
                            cost += p.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
                        }
                        "tool" => { let name = p.get("tool").and_then(Value::as_str).unwrap_or("tool").to_string(); let target = tool_target(p.get("state").and_then(|s| s.get("input"))); tools.push(CliTool { name, target }); }
                        _ => {}
                    }
                }
                return CliParsed { content: content.trim().to_string(), tokens_in: Some(tin.round() as i64).filter(|n| *n > 0), tokens_out: Some(tout.round() as i64).filter(|n| *n > 0), cost: Some(cost).filter(|c| *c > 0.0), tools, error: None };
            }
            // the plain format (an older opencode): the stdout minus its "> agent · model" banner + ANSI
            let ansi = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap();
            let clean = ansi.replace_all(&stdout, "").lines().filter(|l| !l.starts_with("> ")).collect::<Vec<_>>().join("\n").trim().to_string();
            CliParsed { content: clean, ..Default::default() }
        }
        // GEMINI, GROK AND CURSOR: ONE TOLERANT READER, AND IT SAYS SO (2026-09-27). Their ERROR shapes are probed
        // and certain — gemini `{"error":{"message":…,"code":41}}`, grok `{"type":"error","message":…}` — because an
        // unauthenticated call was actually made. Their SUCCESS shapes are not: no seat on this machine has answered
        // one yet. So rather than assert a field name, this reads whichever of the documented content fields is
        // present and falls back to the raw text, which degrades to "the answer, unparsed" instead of to "".
        Engine::Cursor => parse_cursor_stream(&stdout),
        Engine::Gemini => parse_agy_stream(&stdout),
        Engine::Grok => parse_tolerant_json(&stdout),
    }
}

/// CURSOR'S TOOL CALLS, from its stream: every `tool_call` event whose subtype is `started` names the tool by the key of its payload (`readToolCall`, `shellToolCall`, `grepToolCall`, `editToolCall`, ...) and carries the
/// arguments beside it. Named like Claude's tools so the timeline and the audit read one vocabulary. Pure - tested.
pub fn cursor_tools(evs: &[Value]) -> Vec<CliTool> {
    let mut out = vec![];
    for e in evs {
        if e.get("type").and_then(Value::as_str) != Some("tool_call") || e.get("subtype").and_then(Value::as_str) != Some("started") { continue; }
        let Some((key, body)) = e.get("tool_call").and_then(Value::as_object).and_then(|o| o.iter().find(|(_, v)| v.is_object())) else { continue };
        let raw = key.strip_suffix("ToolCall").unwrap_or(key);
        let name = match raw { "shell" => "Bash".to_string(), "" => "tool".to_string(), r => { let mut c = r.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() } };
        out.push(CliTool { name, target: tool_target(body.get("args")) });
    }
    out
}
/// ANTIGRAVITY'S TOOL STEPS, from its stream: a `step_update` of `step_type: "tool"` in state ACTIVE is one call (the same step comes again as DONE or ERROR). The target is the parameter that names what it acts on. Pure - tested.
pub fn agy_tools(evs: &[Value]) -> Vec<CliTool> {
    let mut out = vec![];
    for e in evs {
        let Some(u) = e.get("step_update") else { continue };
        if u.get("step_type").and_then(Value::as_str) != Some("tool") || u.get("state").and_then(Value::as_str) != Some("ACTIVE") { continue; }
        let name = u.get("tool_name").and_then(Value::as_str).unwrap_or("tool").to_string();
        let params = u.get("tool_info").and_then(|i| i.get("parameters"));
        let named = ["CommandLine", "AbsolutePath", "TargetFile", "FilePath", "Path", "SearchPath", "Query", "Pattern", "Url"].iter()
            .find_map(|k| params.and_then(|p| p.get(*k)).and_then(Value::as_str)).map(str::to_string);
        let target = named.or_else(|| params.and_then(Value::as_object).and_then(|o| o.values().find_map(|v| v.as_str().map(str::to_string)))).unwrap_or_default();
        out.push(CliTool { name, target: target.chars().take(200).collect() });
    }
    out
}
/// `agy --output-format stream-json`: tools from the step updates, the answer, tokens and any denied action from the closing `result` event (whose body is what `--output-format json` printed whole). The legacy `gemini`
/// binary prints one plain JSON object and a stream with no `event` lines is read as that.
pub fn parse_agy_stream(stdout: &str) -> CliParsed {
    let evs = json_lines(stdout);
    if !evs.iter().any(|e| e.get("event").is_some()) { return parse_tolerant_json(stdout); }
    let tools = agy_tools(&evs);
    let mut p = match evs.iter().rev().find(|e| e.get("event").and_then(Value::as_str) == Some("result")).and_then(|e| e.get("result")) {
        Some(r) => parse_tolerant_json(&r.to_string()),
        None => CliParsed { error: Some("the CLI printed no result".into()), ..Default::default() },
    };
    p.tools = tools;
    p
}
/// Cursor's stream: the `result` line is the answer (the thinking deltas carry a `text` field too and must never be mistaken for it), the `tool_call` events are the tools. A stream with no `result` line falls back to the
/// tolerant reader over everything that is not a thinking delta.
pub fn parse_cursor_stream(stdout: &str) -> CliParsed {
    let evs = json_lines(stdout);
    let tools = cursor_tools(&evs);
    let kind = |e: &Value| e.get("type").and_then(Value::as_str).unwrap_or("").to_string();
    let result: Vec<String> = evs.iter().filter(|e| kind(e) == "result" || kind(e) == "error").map(|e| e.to_string()).collect();
    let mut p = if !result.is_empty() { parse_tolerant_json(&result.join("\n")) } else {
        let rest: Vec<String> = evs.iter().filter(|e| !matches!(kind(e).as_str(), "thinking" | "tool_call" | "user" | "system")).map(|e| e.to_string()).collect();
        if rest.is_empty() && evs.is_empty() { parse_tolerant_json(stdout) } else { parse_tolerant_json(&rest.join("\n")) }
    };
    // a result that says it failed is the run's error even though its `result` text is the message
    if p.error.is_none() && evs.iter().any(|e| kind(e) == "result" && e.get("is_error").and_then(Value::as_bool) == Some(true)) { p.error = Some(p.content.clone()).filter(|c| !c.is_empty()).or(Some("the CLI reported an error".into())); }
    p.tools = tools;
    p
}
/// The reader for an engine whose success envelope is not yet verified. Tries the whole stdout as one object, then
/// NDJSON (last object wins for content, all objects for tokens), then gives back the raw text.
fn parse_tolerant_json(stdout: &str) -> CliParsed {
    let whole = serde_json::from_str::<Value>(stdout.trim()).ok();
    let evs: Vec<Value> = match &whole { Some(v) => vec![v.clone()], None => json_lines(stdout) };
    if evs.is_empty() {
        let t = stdout.trim();
        return CliParsed { content: t.to_string(), error: if t.is_empty() { Some("the CLI printed nothing".into()) } else { None }, ..Default::default() };
    }
    let mut content = String::new();
    let (mut tin, mut tout, mut cost, mut error) = (0i64, 0i64, 0f64, None::<String>);
    for e in &evs {
        // an error anywhere in the stream is the run's error
        if let Some(msg) = e.get("error").and_then(|x| x.get("message").and_then(Value::as_str).or_else(|| x.as_str())) { error = Some(msg.to_string()); }
        // A DENIED TOOL IS AN ERROR, however cheerful the envelope (2026-09-27). Antigravity reports
        // `status: SUCCESS`, exit 0, an empty `response` and a `denied_actions` list when a tool it needed was
        // auto-denied. Read as an empty answer that is a silent failure; read as an error it is a fixable one, and the
        // bench guards that test for "no answer" can see it.
        if let Some(d) = e.get("denied_actions").and_then(Value::as_array).filter(|a| !a.is_empty()) {
            let names: Vec<String> = d.iter().filter_map(|x| x.get("display_name").or_else(|| x.get("action")).and_then(Value::as_str).map(str::to_string)).collect();
            error = Some(format!("the engine needed a permission this run cannot grant, so it answered nothing (denied: {})",
                                 if names.is_empty() { "unnamed tool".to_string() } else { names.join(", ") }));
        }
        if e.get("type").and_then(Value::as_str) == Some("error") {
            if let Some(m) = e.get("message").and_then(Value::as_str) { error = Some(m.to_string()); }
        }
        // the documented content fields, in the order the three CLIs use them
        for k in ["response", "result", "text", "content"] {
            if let Some(t) = e.get(k).and_then(Value::as_str) { if !t.is_empty() { content = t.to_string(); } }
        }
        for u in [e.get("usage"), e.get("stats"), e.get("usageMetadata")].into_iter().flatten() {
            for k in ["input_tokens", "promptTokenCount", "prompt_tokens", "inputTokens"] { let n = num(u.get(k)); if n > 0 { tin = n; } }
            for k in ["output_tokens", "candidatesTokenCount", "completion_tokens", "outputTokens"] { let n = num(u.get(k)); if n > 0 { tout = n; } }
        }
        for k in ["total_cost_usd", "cost_usd", "cost"] { if let Some(c) = e.get(k).and_then(Value::as_f64) { if c > 0.0 { cost = c; } } }
    }
    // nothing recognised, no error named: hand back the text rather than an empty answer
    if content.trim().is_empty() && error.is_none() { content = stdout.trim().to_string(); }
    CliParsed {
        content: content.trim().to_string(),
        tokens_in: Some(tin).filter(|n| *n > 0), tokens_out: Some(tout).filter(|n| *n > 0),
        cost: Some(cost).filter(|c| *c > 0.0), tools: vec![], error,
    }
}

/// OpenCode narrates its tools on STDERR ("→ Read path", "✱ Grep \"x\" in .", "$ cmd", "✗ cmd failed");
/// the audit and the timeline list them like Claude's and Codex's (2026-09-19: opencode tasks showed
/// 0 tools).
pub fn opencode_tools(stderr: &str) -> Vec<CliTool> {
    let ansi = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap();
    let mut out = vec![];
    for raw in stderr.lines() {
        let l = ansi.replace_all(raw, "").trim().to_string();
        if let Some(rest) = l.strip_prefix("→ ") {
            let (name, target) = rest.split_once(' ').unwrap_or((rest, ""));
            out.push(CliTool { name: name.to_string(), target: target.trim().chars().take(160).collect() });
        } else if let Some(rest) = l.strip_prefix("✱ ") {
            let (name, target) = rest.split_once(' ').unwrap_or((rest, ""));
            out.push(CliTool { name: name.to_string(), target: target.trim().chars().take(160).collect() });
        } else if let Some(rest) = l.strip_prefix("$ ") {
            out.push(CliTool { name: "Bash".into(), target: rest.trim().chars().take(160).collect() });
        }
    }
    out
}

/// The AUDIT of one completion — kept in memory (last 200) so the chat's reasoning lens can list
/// the CLI's own frames under the AI call.
/// THE API-EQUIVALENT OF A TURN A SEAT DID NOT BILL FOR — `None` when there are no tokens to price.
///
/// A flat monthly seat reports no cost because it charges none. Pricing its tokens at the model's published rate
/// is what makes a merge comparable with the raw frontier arm it is measured against, and it is the same basis
/// `fuel::savings_of` already uses everywhere else. Pure — tested.
pub fn priced_from_tokens(engine: Engine, model: Option<&str>, tin: Option<i64>, tout: Option<i64>) -> Option<f64> {
    let (a, b) = (tin.unwrap_or(0).max(0) as f64, tout.unwrap_or(0).max(0) as f64);
    if a <= 0.0 && b <= 0.0 { return None }
    let (pin, pout) = prices_of(engine, model);
    Some((a * pin + b * pout) / 1_000_000.0)
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct CliTrace { pub id: i64, pub engine: Engine, /** the model the call asked for (None = the engine's own default) - what ACTUALLY answered, as the record needs it */ pub model: Option<String>, pub cwd: String, pub started: i64, pub ended: i64, pub ms: i64, pub tools: Vec<CliTool>, #[serde(rename = "tokensIn")] pub tokens_in: Option<i64>, #[serde(rename = "tokensOut")] pub tokens_out: Option<i64>, #[serde(default)] pub reported: Option<f64>, pub cost: Option<f64>, pub chars: usize, pub ok: bool, #[serde(skip_serializing_if = "Option::is_none")] pub error: Option<String> }
impl CliTrace {
    /// A PLAN THAT WAS NOT ASKED FOR AGAIN (2026-09-25). A replayed plan still travels as a trace so every consumer —
    /// the audit, the timeline, the pane — sees a normal turn; it simply carries zero tokens, zero cost and zero
    /// milliseconds, which is exactly what it spent. `ok` is true because a cache hit is a success, not a silence.
    pub fn replayed(engine: Engine) -> CliTrace {
        let t = now_ms();
        CliTrace { model: None, id: 0, engine, cwd: String::new(), started: t, ended: t, ms: 0, tools: vec![],
                   tokens_in: Some(0), tokens_out: Some(0), cost: Some(0.0), reported: Some(0.0), chars: 0, ok: true, error: None }
    }
}
fn traces() -> &'static Mutex<(i64, Vec<CliTrace>)> { static T: OnceLock<Mutex<(i64, Vec<CliTrace>)>> = OnceLock::new(); T.get_or_init(|| Mutex::new((0, Vec::new()))) }
pub fn record_trace(mut t: CliTrace) -> CliTrace {
    let mut g = traces().lock().unwrap_or_else(|p| p.into_inner());
    g.0 += 1; t.id = g.0; g.1.push(t.clone());
    if g.1.len() > 200 { let n = g.1.len() - 200; g.1.drain(0..n); }
    t
}
pub fn traces_since(since_ms: i64) -> Vec<CliTrace> { traces().lock().unwrap_or_else(|p| p.into_inner()).1.iter().filter(|t| t.ended >= since_ms).cloned().collect() }

pub fn now_ms() -> i64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0) }

/// The OpenAI-shaped completion the substrate's ai-call parses.
pub fn completion(model: &str, content: &str, p: &CliParsed) -> Value {
    let (tin, tout) = (p.tokens_in.unwrap_or(0), p.tokens_out.unwrap_or(0));
    let mut usage = json!({ "prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout });
    if let Some(c) = p.cost { usage["cost"] = json!(c); }
    json!({ "id": format!("cli-{}", now_ms()), "object": "chat.completion", "created": now_ms() / 1000, "model": model, "choices": [{ "index": 0, "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }], "usage": usage })
}

/// Linux's MAX_ARG_STRLEN is 131 072 bytes for one argument; leave room for the flags beside it.
const ARGV_PROMPT_MAX: usize = 120_000;

/// Wait for the child held in `slot`. `None` = somebody took it out (a cancel) and killed it. The slot, not a local
/// `Child`, is what a Stop button can reach, so the wait polls it instead of owning the child.
async fn wait_in_slot(slot: &Killer) -> Option<std::io::Result<std::process::ExitStatus>> {
    loop {
        {
            let mut g = slot.lock().unwrap_or_else(|p| p.into_inner());
            match g.as_mut() {
                None => return None,
                Some(c) => match c.try_wait() {
                    Ok(Some(st)) => { g.take(); return Some(Ok(st)); }
                    Ok(None) => {}
                    Err(e) => { g.take(); return Some(Err(e)); }
                },
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
}

pub struct RunOut { pub parsed: CliParsed, pub ms: i64, pub trace: CliTrace }
/// A handle to kill a running CLI (cancel / timeout).
pub type Killer = Arc<Mutex<Option<tokio::process::Child>>>;

fn default_timeout_ms() -> u64 { std::env::var("ANTILOKI_CLI_LLM_TIMEOUT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(300_000) }

/// Run one engine headless in `cwd`; resolves with the parsed answer (and records its trace).
/// `killer`, when given, receives the child so a cancel can SIGKILL it.
pub async fn run_cli(engine: Engine, messages: &[ChatMessage], cwd: &str, wants_json: bool, opts: RunOpts, killer: Option<Killer>) -> Result<RunOut, String> {
    let template = opts.template.clone();
    let ledger = opts.ledger.clone(); let ledger_model = opts.model.clone();
    let tier_v = opts.tier.unwrap_or_default();
    let tier = tier_v.key().to_string();
    let r = run_cli_inner(engine, messages, cwd, wants_json, opts, killer).await;
    if let Some(lctx) = ledger {
        crate::ledger::record_cli(engine, messages, &r, &lctx, ledger_model);
    }
    if let Some(base) = template {
        let hash = crate::prompts::input_hash(&messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n"));
        let (ok, err, tin, tout, cost, ms) = match &r {
            Ok(o) => (o.parsed.error.is_none() && !o.parsed.content.trim().is_empty(), o.parsed.error.clone(), o.parsed.tokens_in.unwrap_or(0), o.parsed.tokens_out.unwrap_or(0), o.trace.cost.unwrap_or(0.0), o.ms),
            Err(e) => (false, Some(e.clone()), 0, 0, 0.0, 0),
        };
        let model = tier_model(engine, tier_v);
        crate::prompts::record(&crate::prompts::CallRecord { base: &base, tier: &tier, engine: engine.key(), model: model.as_deref(), tokens_in: tin, tokens_out: tout, cost_usd: cost, duration_ms: ms, ok, error: err.as_deref(), input_hash: &hash });
    }
    r
}

async fn run_cli_inner(engine: Engine, messages: &[ChatMessage], cwd: &str, wants_json: bool, opts: RunOpts, killer: Option<Killer>) -> Result<RunOut, String> {
    let (system, prompt) = fold_messages(messages, wants_json);
    // THE LAST GATE, BEFORE ANY PROCESS EXISTS (2026-09-27). Every router path is supposed to have asked
    // `engine_usable`/`model_blocked` already; this asks again so that a path which forgot cannot spend a call. That
    // is the difference between "we stopped using Zen" and "Zen is unreachable".
    let asked_model = opts.model.clone().or_else(|| opts.tier.and_then(|t| tier_model(engine, t)));
    if let Some(why) = model_blocked(engine, asked_model.as_deref()) { return Err(why); }
    // GEMINI HAS NO SYSTEM-PROMPT FLAG, so the system text must ride in the prompt itself or it is silently lost.
    let prompt = if engine == Engine::Gemini && !system.is_empty() { format!("{system}\n\n{prompt}") } else { prompt };
    let last_file = if engine == Engine::Codex { Some(std::env::temp_dir().join(format!("antiloki-codex-{}-{}.md", std::process::id(), now_ms())).to_string_lossy().into_owned()) } else { None };
    // Grok reads its prompt from a file, not a pipe. Written here (never by a caller) and removed with the run.
    let prompt_file = if engine.needs_prompt_file() {
        let f = std::env::temp_dir().join(format!("antiloki-prompt-{}-{}.md", std::process::id(), now_ms()));
        match std::fs::write(&f, &prompt) { Ok(()) => Some(f.to_string_lossy().into_owned()), Err(e) => return Err(format!("grok prompt file: {e}")) }
    } else { None };
    let opts = RunOpts { last_message_file: last_file.clone(), prompt_file: prompt_file.clone(),
                         prompt_argv: if engine == Engine::Gemini { Some(prompt.clone()) } else { None }, ..opts };
    // `agy` takes the whole prompt in argv, and Linux refuses a single argument over 128 KiB (E2BIG): say so, instead of
    // a bare "Argument list too long" that reads as a broken seat (audit 2026-10-05).
    if let Some(p) = &opts.prompt_argv { if p.len() > ARGV_PROMPT_MAX { return Err(format!("{} takes its prompt in argv and this one is {} KB (limit {} KB): shorten the task or route it to a seat that reads stdin", engine.key(), p.len() / 1024, ARGV_PROMPT_MAX / 1024)); } }
    let (bin, args) = cli_command(engine, &system, &opts);
    let mut cmd = match &opts.wall {
        // THE WALL (wall.rs): the same call, run by `bwrap`; the two files a CLI reads or writes under /tmp are let through it
        Some(w) => { let mut w = w.clone(); for f in [&last_file, &prompt_file].into_iter().flatten() { if !std::path::Path::new(f).exists() { let _ = std::fs::write(f, ""); } w.files.push(f.into()); } let real = crate::proc::which(bin).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| bin.to_string()); let mut c = crate::proc::tokio_cmd("bwrap"); c.args(crate::wall::args(&w, &real, &args)); c }
        None => { let mut c = crate::proc::tokio_cmd(bin); c.args(&args); c }
    };
    cmd.current_dir(cwd).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    // a server started from inside a Claude session inherits its markers — the child must not think it is nested
    cmd.env("CI", "1").env("BROWSER", "none");
    for k in ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_ENTRYPOINT"] { cmd.env_remove(k); }
    // READ-ONLY: Claude and Codex are gated by their flags; OpenCode has no run-time sandbox flag,
    // so its permissions are denied through its config env for a read-only run.
    // A headless run is a SUB-AGENT (a goal-run task, a brief, the verifier, an X-ray call): it never
    // delegates again. The MCP sidecar refuses under this env; OpenCode additionally loses the
    // antiloki server from the worktree's opencode.json (2026-09-19: a deepseek sub-task found it and
    // spawned its own run — recursion). Claude/Codex headless runs carry no MCP wiring to begin with.
    cmd.env("ANTILOKI_SUBAGENT", "1");
    if engine == Engine::Opencode {
        let mut cfg = json!({ "mcp": { "antiloki": { "enabled": false } } });
        // bash "deny" (2026-09-19, probed on the Go plan with deepseek-v4-flash): the model gets a tool error and
        // finishes its report; "ask" auto-rejects headless and ENDS the run with no answer ("no report").
        // A write run gets the shell like Claude (Bash) and Codex (workspace-write) do — the pane's
        // "bash: ask" would auto-reject headless and end the run the same silent way (2026-09-19).
        cfg["permission"] = if opts.write { json!({ "edit": "allow", "bash": "allow", "webfetch": "allow" }) } else { json!({ "edit": "deny", "bash": "deny", "webfetch": "allow" }) };
        cmd.env("OPENCODE_CONFIG_CONTENT", cfg.to_string());
    }
    for (k, v) in &opts.env { cmd.env(k, v); }
    let t0 = now_ms();
    let timeout = std::time::Duration::from_millis(opts.timeout_ms.unwrap_or_else(default_timeout_ms));
    let fail = |e: String| { record_trace(CliTrace { model: asked_model.clone(), id: 0, engine, cwd: cwd.into(), started: t0, ended: now_ms(), ms: now_ms() - t0, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: false, error: Some(e.clone()) }); e };
    let mut child = cmd.spawn().map_err(|e| fail(format!("{bin}: {e}")))?;
    let pid = child.id().map(|p| p as i32).unwrap_or(0);
    let _headless = child.id().map(|p| crate::mem::headless_register(p as i32));   // the machine's watcher may pause this run while the person's own programs need the room (mem.rs)
    if let Some(mut stdin) = child.stdin.take() { let _ = stdin.write_all(prompt.as_bytes()).await; drop(stdin); }
    let stdout = child.stdout.take(); let stderr = child.stderr.take();
    // THE CHILD LIVES IN THE KILLER SLOT FOR THE WHOLE RUN (audit 2026-10-05: nothing ever put it there, so Stop
    // killed no real CLI on any seat and `goals.rs` "alive" was always false). A caller without a handle gets a
    // private slot, so the timeout path below is the same code.
    let slot: Killer = killer.clone().unwrap_or_else(|| Arc::new(Mutex::new(None)));
    *slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(child);
    // stdout is read in chunks so a live tap can see each tool use as it streams; the whole text is kept exactly as before
    let live = opts.live.clone(); let line_tap = opts.line_tap.clone();
    let read_all = |r: Option<tokio::process::ChildStdout>| async move { let mut s = String::new(); if let Some(mut r) = r { let mut buf = vec![0u8; 8192]; let mut pending = String::new(); let mut bytes: Vec<u8> = vec![]; loop { match tokio::io::AsyncReadExt::read(&mut r, &mut buf).await { Ok(0) | Err(_) => break, Ok(n) => { bytes.extend_from_slice(&buf[..n]); if live.is_some() || line_tap.is_some() { pending.push_str(&String::from_utf8_lossy(&buf[..n])); while let Some(i) = pending.find('\n') { let line: String = pending.drain(..=i).collect(); if let Some(tap) = &live { for (t, g) in live_tools_of_line(&line) { tap(&t, &g); } } if let Some(lt) = &line_tap { lt(&line); } } } } } } s = String::from_utf8_lossy(&bytes).to_string(); } s };
    let read_err = |r: Option<tokio::process::ChildStderr>| async move { let mut s = String::new(); if let Some(mut r) = r { let _ = tokio::io::AsyncReadExt::read_to_string(&mut r, &mut s).await; } s };
    let (out, err, status) = tokio::select! {
        r = async { let (o, e) = tokio::join!(read_all(stdout), read_err(stderr)); let st = wait_in_slot(&slot).await; (o, e, st) } => r,
        held = crate::mem::run_time(timeout, std::time::Duration::from_secs(1), move || crate::mem::headless_paused(pid)) => {
            // a timeout keeps what the CLI streamed so far (its tools stay in the audit) and reports as such
            let taken = slot.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(mut c) = taken { let _ = c.kill().await; }
            let parsed = CliParsed { error: Some(if held.as_secs() > 0 { format!("timed out after {} s of running (it was also held {} s because the machine was busy)", timeout.as_secs(), held.as_secs()) } else { format!("timed out after {} s", timeout.as_secs()) }), ..Default::default() };
            let ended = now_ms();
            let trace = record_trace(CliTrace { model: asked_model.clone(), id: 0, engine, cwd: cwd.into(), started: t0, ended, ms: ended - t0, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: false, error: parsed.error.clone() });
            crate::audit::from_trace_tiered(&trace, opts.model.as_deref(), opts.tier, opts.template.as_deref(), opts.run_id);
            return Ok(RunOut { parsed, ms: ended - t0, trace });
        }
    };
    // `None` = a cancel took the child out of the slot and killed it: whatever it printed is not an answer
    let Some(status) = status else {
        if let Some(f) = &last_file { let _ = std::fs::remove_file(f); }
        if let Some(f) = &prompt_file { let _ = std::fs::remove_file(f); }
        return Err(fail(format!("{} was cancelled", engine.key())));
    };
    let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
    if code != 0 && out.trim().is_empty() { return Err(fail(format!("{} exited {code}: {}", engine.key(), err.trim().chars().rev().take(600).collect::<String>().chars().rev().collect::<String>()))); }
    let last = last_file.as_ref().and_then(|f| { let t = std::fs::read_to_string(f).ok(); let _ = std::fs::remove_file(f); t });
    if let Some(f) = &prompt_file { let _ = std::fs::remove_file(f); }   // the prompt is not an artefact; it goes with the run
    if engine == Engine::ClaudeCode {
        if let Some(root) = opts.meter_root.as_deref() {
            for ev in json_lines(&out) { if matches!(ev.get("type").and_then(Value::as_str), Some("assistant" | "result" | "rate_limit_event")) { crate::agent::note_usage(std::path::Path::new(root), 0, &ev); } }
        }
    }
    let mut parsed = parse_cli_output(engine, &out, last.as_deref());
    if engine == Engine::Opencode && parsed.tools.is_empty() { parsed.tools = opencode_tools(&err); }
    // exit 0 with nothing to report: say what the CLI said on stderr instead of a bare "no report"
    // (2026-09-19: OpenCode's auto-rejected bash ended a run silently)
    if parsed.content.trim().is_empty() && parsed.error.is_none() {
        let tail = err.trim().lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" · ");
        parsed.error = Some(if tail.is_empty() { format!("{} produced no answer (exit {code})", engine.key()) } else { format!("{} produced no answer (exit {code}): {}", engine.key(), tail.chars().take(500).collect::<String>()) });
    }
    let ended = now_ms();
    // A SEAT THAT CHARGES NOTHING STILL HAS TO BE PRICED (found 2026-09-28 on the pipeline's first SWT-bench arm).
    //
    // Cursor and Antigravity report no `total_cost_usd`, because on their own pools there is no token rate to
    // report. `parsed.cost` was therefore `None`, the task trace carried `cost: null`, and the bench's own
    // accounting — which sums `plan_usd` plus the task costs — priced a ten-instance run at the PLANNER ALONE.
    // The headline would have read "$0.100 an instance, 3.7x cheaper than raw Sonnet" with the entire worker
    // side uncounted. That is the exact shape this repo's benchmark doctrine warns about, and it flattered us.
    //
    // The tokens ARE reported, so the API-equivalent is computable and is the basis every saving here already
    // uses: what these tokens would have cost at the model's published rate. It is not what the operator paid —
    // they paid a flat monthly seat — which is the whole point, and is why `savings` can speak at all.
    // …AND A REPORTED ZERO IS AN ABSENT MEASUREMENT, NOT A FREE TURN (audit 2026-09-28). `or_else` only fills
    // `None`, and the claude-envelope parser records `total_cost_usd: 0` as `Some(0.0)` — which is exactly what
    // `cursor-agent` sends, being the seat that charges nothing per token. The hole was at the one parser the fix
    // was written for. Everywhere else in this file already treats 0 as absent (`filter(|c| *c > 0.0)`).
    let reported = parsed.cost.filter(|c| *c > 0.0);
    let cost = reported.or_else(|| priced_from_tokens(engine, opts.model.as_deref(), parsed.tokens_in, parsed.tokens_out));
    let trace = record_trace(CliTrace { model: asked_model.clone(), id: 0, engine, cwd: cwd.into(), started: t0, ended, ms: ended - t0, tools: parsed.tools.clone(), tokens_in: parsed.tokens_in, tokens_out: parsed.tokens_out, cost, reported, chars: parsed.content.len(), ok: !parsed.content.trim().is_empty(), error: parsed.error.clone() });
    // THE AUDIT (2026-09-23): every call, its model, its deterministic price, its timer — one row, feeding the merges
    crate::audit::from_trace_tiered(&trace, opts.model.as_deref(), opts.tier, opts.template.as_deref(), opts.run_id);
    Ok(RunOut { parsed, ms: ended - t0, trace })
}

#[cfg(test)]
mod seat_off_tests {
    use super::*;
    #[test]
    fn a_role_bound_to_a_seat_that_is_gone_goes_to_the_seat_the_benchmark_trusts() {
        let bound = Binding { engine: Engine::ClaudeCode, model: Some("opus".into()), source: "role" };
        let only = |keep: &'static [Engine]| move |e: Engine, _m: Option<&str>| keep.contains(&e);
        // everything is up: the operator's binding stands
        assert_eq!(binding_when_unavailable(Role::Qa, bound.clone(), Engine::ClaudeCode, |_, _| true, || None), bound);
        // Claude is off: the QA goes to Cursor's Grok-high (0 false accepts of 50 on the first set), then Composer
        let b = binding_when_unavailable(Role::Qa, bound.clone(), Engine::ClaudeCode, only(&[Engine::Cursor, Engine::Gemini, Engine::Codex]), || Some(Engine::Gemini));
        assert_eq!((b.engine, b.model.as_deref(), b.source), (Engine::Cursor, Some("cursor-grok-4.6-high"), "unavailable-seat"));
        // Cursor is out too: Gemini is the last resort before Codex - never first
        let b = binding_when_unavailable(Role::Qa, bound.clone(), Engine::ClaudeCode, only(&[Engine::Gemini, Engine::Codex]), || Some(Engine::Codex));
        assert_eq!(b.engine, Engine::Gemini);
        // a non-QA role goes where Heimdall itself would plan
        let p = binding_when_unavailable(Role::Planner, Binding { engine: Engine::ClaudeCode, model: Some("sonnet".into()), source: "role" }, Engine::ClaudeCode, only(&[Engine::Codex, Engine::Gemini]), || Some(Engine::Gemini));
        assert_eq!(p.engine, Engine::Gemini);
        // nothing at all is available: the run's own engine stands in (the call then fails with its own honest error)
        let n = binding_when_unavailable(Role::Qa, bound, Engine::Cursor, |_, _| false, || None);
        assert_eq!(n.engine, Engine::Cursor);
    }
    #[test]
    fn a_call_naming_a_gone_seat_runs_on_the_seat_heimdall_would_use() {
        let up = |keep: &'static [Engine]| move |e: Engine| keep.contains(&e);
        // the seat asked for can take the call: it stands, Heimdall is not consulted
        assert_eq!(seat_or_heimdall_with(Engine::Codex, up(&[Engine::Codex]), || panic!("not consulted")), Some(Engine::Codex));
        // Claude is off: the call goes where Heimdall plans
        assert_eq!(seat_or_heimdall_with(Engine::ClaudeCode, up(&[Engine::Codex, Engine::Gemini]), || Some(Engine::Codex)), Some(Engine::Codex));
        // nothing can run: None, so the caller keeps its own seat and gets the honest refusal
        assert_eq!(seat_or_heimdall_with(Engine::ClaudeCode, up(&[]), || None), None);
    }
    #[test]
    fn a_seat_the_operator_switched_off_is_refused_everywhere_and_comes_back_when_switched_on() {
        // Grok stands in: no other test routes through it
        set_provider_flag("grok", true).unwrap();
        assert!(!seat_off(Engine::Grok) && model_blocked(Engine::Grok, None).is_none());
        set_provider_flag("grok", false).unwrap();
        assert!(seat_off(Engine::Grok));
        assert_eq!(model_blocked(Engine::Grok, None).as_deref(), Some("Grok is switched off (Settings › Seats)."));
        assert!(!engine_usable(Engine::Grok) && !engine_routable(Engine::Grok), "the router never picks it");
        assert!(!seat_off(Engine::ClaudeCode), "the others are untouched");
        set_provider_flag("grok", true).unwrap();
        assert!(!seat_off(Engine::Grok) && model_blocked(Engine::Grok, None).is_none());
        assert!(set_provider_flag("nonsense", true).is_err());
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    #[test]
    fn a_quota_message_is_a_limit_and_nothing_else_is() {
        for m in ["You've hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at 4:44 PM.",
                  "Claude usage limit reached. Your limit will reset at 7pm (UTC).", "HTTP 429 Too Many Requests", "RESOURCE_EXHAUSTED: quota exceeded for this model", "codexErrorInfo: usageLimitExceeded"] {
            assert!(is_limit_error(m), "{m}");
        }
        for m in ["prompt is too long: context length exceeded", "request timed out", "unknown model opus-9", "not logged in", "produced no answer (exit 1): segfault"] {
            assert!(!is_limit_error(m), "{m}");
        }
    }
    #[test]
    fn a_lapsed_subscription_is_remembered_like_a_limit_but_a_timeout_is_not() {
        // OpenCode stands in: no other test notes a limit on it
        clear_limit(Engine::Opencode);
        assert!(!note_limit(Engine::Opencode, "request timed out") && seat_limit(Engine::Opencode).is_none());
        assert!(note_limit(Engine::Opencode, "claude-code exited 1: Not logged in · Please run /login") && seat_limit(Engine::Opencode).is_some());
        clear_limit(Engine::Opencode);
        assert!(seat_limit(Engine::Opencode).is_none());
    }
    #[test]
    fn a_message_that_says_when_it_resets_is_believed() {
        let g = "Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 38m26s.";
        assert!(is_limit_error(g), "Gemini's wording is a limit");
        assert_eq!(limit_reset_in_ms(g), Some((38 * 60 + 26) * 1000));
        assert_eq!(limit_reset_in_ms("rate limited, try again in 2h 5m"), Some((2 * 3600 + 5 * 60) * 1000));
        assert_eq!(limit_reset_in_ms("retry in 90 seconds please"), Some(90_000));
        assert_eq!(limit_reset_in_ms("usage limit, try again at 4:44 PM"), None, "a clock time gives no duration");
        assert_eq!(limit_window_ms(g), (38 * 60 + 26) * 1000 + 5_000, "the window is the message's own time, plus a margin");
    }
    #[test]
    fn a_date_in_the_sentence_is_a_weekly_window_and_a_clock_time_is_a_session() {
        assert_eq!(limit_window_ms("usage limit. try again at 4:44 PM."), SESSION_LIMIT_MS);
        assert_eq!(limit_window_ms("You've hit your usage limit ... or try again at Oct 9th, 2026 6:39 PM."), WEEKLY_LIMIT_MS);
        assert_eq!(limit_window_ms("you have hit your weekly limit"), WEEKLY_LIMIT_MS);
        assert_eq!(limit_window_ms("usage limit; try again at 4:44 PM, the march of time"), SESSION_LIMIT_MS, "a word that merely starts like a month is not a date");
    }
    #[test]
    fn a_nearly_full_window_takes_the_seat_out_before_the_call_and_a_passed_one_does_not() {
        let w = |h: &str, l: &str, u: f64, r: Option<i64>| Window { house: h.into(), label: l.into(), used_pct: u, resets_at_s: r };
        let ws = vec![w("codex", "5h", 100.0, Some(2000)), w("codex", "week", 78.0, Some(9000)), w("claude", "5h", 94.9, Some(2000)), w("claude", "week", 96.0, Some(500))];
        let (pct, why, until) = full_window(&ws, Engine::Codex, 1000).expect("codex is full");
        assert_eq!(pct, 100.0); assert!(why.contains("5-hour") && why.contains("100%") && why.contains("17 min"), "{why}"); assert_eq!(until, 2_000_000);
        assert!(full_window(&ws, Engine::ClaudeCode, 1000).is_none(), "94.9% is enough, and the 96% weekly window has already reset");
        assert!(full_window(&ws, Engine::Gemini, 1000).is_none(), "a seat with no windows is never blocked by them");
        assert!(full_window(&ws, Engine::Codex, 2500).is_none(), "the window reset: the seat is back");
    }
    #[test]
    fn a_limited_seat_comes_back_when_its_window_is_over_and_when_it_answers() {
        let mut l = Limits::default();
        l.note(Engine::Codex, "You've hit your usage limit. try again at 4:44 PM", 1_000);
        assert!(l.get(Engine::Codex, 1_000 + SESSION_LIMIT_MS - 1).is_some());
        assert!(l.get(Engine::Codex, 1_000 + SESSION_LIMIT_MS + 1).is_none(), "the window ended");
        assert!(l.get(Engine::Gemini, 2_000).is_none(), "another seat is untouched");
        l.note(Engine::Codex, "usage limit", 5_000); l.clear(Engine::Codex);
        assert!(l.get(Engine::Codex, 5_001).is_none());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_seat_nobody_named_is_the_persons_own_not_claudes() {
        use super::{seat_for, Engine};
        let only = |a: Engine| move |e: Engine| e == a;
        assert_eq!(seat_for("", None, "pane", only(Engine::Cursor)), Engine::Cursor);
        assert_eq!(seat_for("claude-code", None, "pane", only(Engine::Cursor)), Engine::Cursor, "a seat this machine cannot use");
        assert_eq!(seat_for("", Some(Engine::Cursor), "pane", |_| true), Engine::Cursor, "the asking agent's own");
        assert_eq!(seat_for("codex", Some(Engine::Cursor), "pane", |_| true), Engine::Codex, "a named seat stands");
        assert_eq!(seat_for("claude-code:haiku", None, "pane", |_| true), Engine::ClaudeCode);
        assert_eq!(seat_for("", None, "codex", |_| true), Engine::Codex); assert_eq!(seat_for("", None, "pane", |_| true), Engine::ClaudeCode);
    }
    /// PLAN P2's PROBE — not a check, a measurement: the editor winner per domain on a COPY of the live record, blind vs domain-aware.
    /// `PROBE_DB=~/.antiloki/merges.sqlite cargo test -p antiloki-serve --lib probe_p2 -- --ignored --nocapture`
    #[test]
    #[ignore = "reads a copy of the operator's live record; prints, asserts nothing about it"]
    fn probe_p2_the_editor_winner_per_domain() {
        let src = std::env::var("PROBE_DB").expect("PROBE_DB=<path to merges.sqlite>");
        let dst = crate::merges::path(); std::fs::create_dir_all(dst.parent().unwrap()).unwrap(); std::fs::copy(&src, &dst).unwrap();
        for size in ["l", "s"] {
            let job = if size == "s" { Job::EditorS } else { Job::EditorL };
            println!("── {size}  blind: {:?}", crate::merges::best_for(job, size, 3));
            for d in ["backend", "frontend", "design", "docs", "infra", "mixed", "tests"] {
                println!("   {d:9} domain-aware: {:?}", learned_winner(job, size, Some(d)));
            }
        }
    }

    /// ITEM 4 (2026-10-01): THE EDITOR OF A SMALL RUN IS CHOSEN AMONG THE SOLO CELLS AND THE PAIRS, and the run can hear why. A seat
    /// measured ALONE that keeps the band and costs less wins the small sizes; the brain jobs never get a solo seat (there is no
    /// brain in a `raw` arm); and above L nothing changes.
    #[test]
    fn the_editor_of_a_small_run_is_chosen_among_solo_cells_and_pairs_and_the_brain_jobs_never_get_a_solo_seat() {
        let _ = std::fs::remove_file(crate::merges::path());
        let row = |size: &str, planner: &str, worker: &str, audit: f64, usd: f64| crate::merges::Outcome { suite: "fix".into(), stack: "any".into(), job: "editor-l".into(), size: size.into(),
            domain: "backend".into(), planner: planner.into(), worker: worker.into(), qa: String::new(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0,
            audit: Some(audit), ms: 60_000, usd, source: "bench".into(), audit_only: false };
        for _ in 0..3 {
            crate::merges::record(&row("l", "claude-code:sonnet", "cursor:composer-2.5", 0.95, 0.21)).unwrap();
            crate::merges::record(&row("l", "raw", "cursor:cursor-grok-4.6-medium", 0.90, 0.02)).unwrap();
            crate::merges::record(&row("xxl", "claude-code:sonnet", "cursor:composer-2.5", 0.95, 0.21)).unwrap();
            crate::merges::record(&row("xxl", "raw", "cursor:cursor-grok-4.6-medium", 0.95, 0.01)).unwrap();
        }
        // with the domain the files name, and without it
        for d in [Some("backend"), None] {
            let (e, m, why) = learned_pick(Job::EditorL, "l", d).expect("a winner");
            assert_eq!((e, m.as_str()), (Engine::Cursor, "cursor-grok-4.6-medium"), "the solo seat wins the small size ({d:?})");
            assert!(why.contains("solo wins"), "the run hears why: {why}");
            assert_eq!(learned_winner(Job::EditorL, "l", d).map(|(e, m)| (e, m)), Some((Engine::Cursor, "cursor-grok-4.6-medium".to_string())));
        }
        // above L the planner is part of the job: the pair's hands, and no sentence about solo cells
        let (e, m, why) = learned_pick(Job::EditorL, "xxl", Some("backend")).expect("a winner");
        assert_eq!((e, m.as_str()), (Engine::Cursor, "composer-2.5"));
        assert!(why.is_empty(), "{why}");
        // the brain jobs read the pairs only — a `raw` arm has no brain to name
        let (e, m, _) = learned_pick(Job::Planner, "l", None).expect("a planner");
        assert_eq!((e, m.as_str()), (Engine::ClaudeCode, "sonnet"));
    }

    /// "ALWAYS ONE WORKER CHEAPER THAN ITS PLANNER" IS A MERGE'S RULE (item 4, 2026-10-01): a run that called no planner keeps the seat the
    /// record chose; a merge's worker is held under the planner exactly as before.
    #[test]
    fn a_solo_run_is_not_held_under_the_price_of_a_planner_it_never_called() {
        let bind = |m: &str| Binding { engine: Engine::ClaudeCode, model: Some(m.into()), source: "learned" };
        let (w, note) = worker_for_run(bind("opus"), &bind("haiku"), true, |_| true);
        assert_eq!(w.model.as_deref(), Some("opus"), "a solo run keeps the seat that won on value");
        assert!(note.is_none());
        // a merge is untouched: whatever `worker_under_planner` does, `worker_for_run` does
        let (a, an) = worker_for_run(bind("opus"), &bind("haiku"), false, |_| true);
        let (b, bn) = worker_under_planner(bind("opus"), &bind("haiku"));
        assert_eq!((a.engine, a.model, an), (b.engine, b.model, bn));
        // and a seat that cannot take a call is still moved off, solo or not
        let (c, cn) = worker_for_run(bind("opus"), &bind("haiku"), true, |_| false);
        let (d, dn) = worker_under_planner(bind("opus"), &bind("haiku"));
        assert_eq!((c.engine, c.model, cn), (d.engine, d.model, dn));
    }

    /// A TEST THAT NEEDS THE GATEWAY SAYS SO, AND CANNOT LEAK IT (2026-09-27). Zen is off by default, which is the
    /// product's policy; several tests below exercise the routing ARITHMETIC and need a reachable OpenCode to do it.
    /// Resetting in `Drop` matters because the per-thread models file is shared by every test on that thread, so a
    /// panicking test would otherwise leave the gateway switched on for its neighbours.
    struct ZenOn;
    impl ZenOn {
        fn on() -> Self { super::set_provider_flag("zen", true).expect("the switch is writable"); ZenOn }
    }
    impl Drop for ZenOn {
        fn drop(&mut self) { let _ = super::set_provider_flag("zen", false); }
    }

    /// A LABEL IS NOT A BINDING — the SWE-bench lesson, at the layer that launches things.
    /// EVERY ENGINE MUST BE KNOWN TO THE SEAT CHECK (2026-09-26). `engine_seat` answering `None` means "we do not
    /// know how to tell whether this provider is signed in", and `engine_usable` deliberately treats that as "do
    /// not deny" — so a new engine added without its credential path would be offered to the operator on the
    /// strength of its binary alone, which is the exact bug the seat check exists to prevent. This test fails the
    /// moment an engine is added and its seat path is forgotten.
    ///
    /// It asserts only that the answer is KNOWN, never what it is: the answer depends on whose machine this runs on.
    /// A PAIR CONSTRAINS THE ENGINE, NOT THE RECORD (2026-09-26). `job_binding_learned` returned early on any pair,
    /// dropping `size` and falling through to the size-blind `job_defaults` — so under a merge nothing measured was
    /// consulted at all. The winner is now asked for WITHIN the side's engine: it can move the model, never the engine,
    /// and a pair that NAMES a model is an instruction that stands.
    #[test]
    fn a_pair_pins_the_engine_and_still_lets_the_record_choose_the_model() {
        // PORTED OFF THE ZEN PAIRS (2026-09-27): `claude-deepseek` was withdrawn with the rest of the metered
        // merges, so this property is now asserted on `claude-gemini`, which has the same shape that matters here —
        // a pinned brain engine and a hands side that names NO model, so the record is still free to choose it.
        let gemini_composer = Pair::builtin("gemini-composer").expect("a built-in pair");
        assert!(gemini_composer.brain_model.is_none(), "this test needs a side whose model the record picks");
        // the brain side is Gemini; whatever the record says, the engine must stay Gemini. Asked at `l`, because
        // below that the merge does not split at all any more (`pair_splits_at`) and there is no brain to pin.
        let (w, _f) = job_binding_learned(Job::Planner, "l", Some(&gemini_composer), Engine::ClaudeCode, None);
        assert_eq!(w.engine, Engine::Gemini, "the pair's brain engine is absolute: {w:?}");
        // the hands side is Cursor, likewise
        let (hw, _) = job_binding_learned(Job::EditorL, "l", Some(&gemini_composer), Engine::ClaudeCode, None);
        assert_eq!(hw.engine, Engine::Cursor, "the pair's hands engine is absolute: {hw:?}");

        // a pair that NAMES its models is an instruction — the record must not move either side
        let named = Pair::builtin("grok-composer").expect("a built-in pair that names both sides");
        assert!(named.brain_model.is_some() && named.hands_model.is_some());
        // at `l`, where the brain half exists at all — below it the pair collapses to the hands by design
        let (nb, _) = job_binding_learned(Job::Planner, "l", Some(&named), Engine::ClaudeCode, None);
        assert_eq!(nb.model.as_deref(), named.brain_model.as_deref(), "a named brain model stands: {nb:?}");
        let (nh, _) = job_binding_learned(Job::EditorL, "l", Some(&named), Engine::ClaudeCode, None);
        assert_eq!(nh.model.as_deref(), named.hands_model.as_deref(), "a named hands model stands: {nh:?}");

        // A SMALL TASK IS NOT MERGED (operator 2026-09-28: "a small task like add a button is getting split into
        // brain and worker. But we said only L and above gets the split merge and all below gets only the worker +
        // QA"). Below `l` the brain half is dropped and the hands do the work alone.
        let small = job_binding_learned(Job::Planner, "s", Some(&gemini_composer), Engine::ClaudeCode, None).0;
        assert_eq!(small.engine, Engine::Cursor, "a small plan runs on the hands, not on a second model: {small:?}");
        for sz in ["s", "xs", "call"] {
            assert!(!pair_splits_at(sz), "{sz} must not split");
            assert_eq!(job_binding_learned(Job::Planner, sz, Some(&gemini_composer), Engine::ClaudeCode, None).0.engine,
                       Engine::Cursor, "{sz}");
        }
        for sz in ["l", "xxl", " L "] { assert!(pair_splits_at(sz), "{sz} must split"); }

        // …AND QA IS NOT THE MERGE'S BRAIN. It is the check that the merge was right, so a small task keeps its
        // judge — "the worker + QA" is the operator's own phrasing of what small work gets.
        let qa_small = job_binding_learned(Job::QaGate, "s", Some(&gemini_composer), Engine::ClaudeCode, None).0;
        assert_eq!(qa_small.engine, Engine::Gemini, "QA keeps the brain side at every size: {qa_small:?}");

        // and `size` now actually reaches the lookup — the whole point of the change
        let s_side = job_binding_learned(Job::EditorS, "s", Some(&gemini_composer), Engine::ClaudeCode, None).0;
        let l_side = job_binding_learned(Job::EditorL, "l", Some(&gemini_composer), Engine::ClaudeCode, None).0;
        assert_eq!((s_side.engine, l_side.engine), (Engine::Cursor, Engine::Cursor), "both are hands work");
    }

    #[test]
    fn every_engine_knows_where_its_seat_lives() {
        for e in Engine::all() {
            assert!(engine_seat(e).is_some(), "{} has no seat check — add its credential path to engine_seat", e.key());
        }
    }

    #[test]
    fn a_record_key_becomes_a_binding_only_when_it_names_an_engine_and_a_model() {
        assert_eq!(binding_of_key("claude-code:sonnet"), Some((Engine::ClaudeCode, "sonnet".into())));
        assert_eq!(binding_of_key("opencode:opencode/qwen3.6-plus"), Some((Engine::Opencode, "opencode/qwen3.6-plus".into())));
        // the yardstick and the solo marker are not engines
        assert_eq!(binding_of_key("raw"), None);
        assert_eq!(binding_of_key("-"), None);
        // THE EXACT STRING SWE-BENCH RECORDED AS A WINNER, which the router could have picked and then failed to
        // launch: it has no `:` and names no engine this build knows
        assert_eq!(binding_of_key("antiloki-opencode-qwen3.6-plus"), None);
        assert_eq!(binding_of_key("mystery:1"), None);
        assert_eq!(binding_of_key("claude-code:"), None, "an engine with no model is half a binding");
        assert_eq!(binding_of_key(""), None);
    }

    /// A FALLBACK THAT IS THE WINNER IS NOT A FALLBACK (2026-09-26). Two bench runs died on a dead DeepSeek gateway
    /// with no second attempt, because the pair named the model the job cell already held and both sides came back
    /// identical — so `run_job` declined to retry the thing that had just failed, which is the right rule applied to
    /// a wrong pair of bindings.
    #[test]
    fn a_pair_whose_model_is_also_the_cells_winner_still_gets_a_second_rung() {
        // PORTED to Cursor's own pool (2026-09-27) — same shape: one engine on both sides, both models named.
        let p = Pair::builtin("grok-composer").expect("both sides on Cursor, both named");
        let (w, f) = job_binding_paired(Job::Planner, Some(&p), Engine::Cursor, None);
        assert_eq!(w.model.as_deref(), Some("cursor-grok-4.6-high"), "the pair's brain still wins");
        assert_eq!(w.engine, Engine::Cursor);
        assert_ne!(f.model, w.model, "and there is something else to try when it dies");
        assert_eq!(f.engine, Engine::Cursor, "the pair chose the engine; a fallback stays inside it");
    }

    use super::*;

    #[test]
    fn a_pair_survives_its_own_round_trip() {
        // the FE's shape
        let fe = json!({ "pair": { "name": "frontender-best",
            "brain": { "engine": "claude-code", "model": "sonnet" },
            "hands": { "engine": "opencode", "model": "opencode/deepseek-v4-flash" } } });
        let a = Pair::of_settings(&fe).expect("the FE shape parses");
        assert_eq!(a.brain_model.as_deref(), Some("sonnet"));
        assert_eq!(a.hands_model.as_deref(), Some("opencode/deepseek-v4-flash"));

        // serialise it the way the recipe store and every pane's settings do, and read it back
        let stored = json!({ "pair": serde_json::to_value(&a).unwrap() });
        let b = Pair::of_settings(&stored).expect("its OWN serialisation must parse — this is the bug (2026-09-24)");
        assert_eq!(b.name, a.name);
        assert_eq!(b.brain, a.brain);
        assert_eq!(b.hands, a.hands);
        assert_eq!(b.brain_model, a.brain_model);
        assert_eq!(b.hands_model, a.hands_model);
        assert_eq!(b.label(), a.label(), "and it reads as the same merge, not as solo");

        // a genuinely solo pane is still solo
        assert!(Pair::of_settings(&json!({ "pair": "solo" })).is_none());
        assert!(Pair::of_settings(&json!({})).is_none());
    }

    use super::*;
    fn m(role: &str, c: &str) -> ChatMessage { ChatMessage { role: role.into(), content: c.into() } }
    /// ENV IS PROCESS-GLOBAL: `tiers_become_model_flags_per_engine` sets ANTILOKI_TIER_MODEL_* for a moment, and
    /// every test that reads the ladder saw the gap (flaked 2 in 3 on 2026-09-22). They take turns.
    pub(super) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        L.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn list_prices_and_model_switches_cover_every_engine_and_tier() {
        for e in Engine::all() { for t in [Tier::Low, Tier::Mid, Tier::High] { let (m, i, o) = list_price(e, t); assert!(!m.is_empty() && i > 0.0 && o > i, "{e:?} {t:?}"); assert!(!model_switch(e, t).is_empty()); } }
        assert!(list_price(Engine::ClaudeCode, Tier::High).1 > list_price(Engine::ClaudeCode, Tier::Mid).1, "high costs more than mid");
        assert_eq!(model_switch(Engine::ClaudeCode, Tier::Low), "/model haiku"); assert_eq!(model_switch(Engine::ClaudeCode, Tier::Mid), "/model sonnet");
    }
    #[test]
    fn the_models_bar_moves_every_role_one_rung_and_stops_at_the_ends() {
        let _env = env_lock();
        // the operator set nothing: the ladder is the built-in one for claude-code
        assert_eq!(ladder(Engine::ClaudeCode), vec!["haiku", "sonnet"]);   // never Opus
        let at = |r: Role, c: Option<u8>| role_binding_at(r, Engine::ClaudeCode, c).model.unwrap_or_default();
        // BALANCED is exactly what the roles bind to today — the bar in the middle changes NOTHING
        // THE QA RUNG IS SONNET. It went Opus -> sonnet (wrongly, on SPEED) -> haiku (wrongly, on PRICE) -> sonnet,
        // on the argument that settles it: the gate is the last line, so it is chosen on accuracy alone.
        // the gate is a MEASURED seat (`qa_gate_ranking`): when any seat of the ranking can take a call the dial does not move it; with none the tier rule below applies
        let ranked = qa_gate_pick(None, seat_ok);
        let qa_bal: String = ranked.as_ref().and_then(|g| g.model.clone()).unwrap_or_else(|| "sonnet".into());
        assert_eq!((at(Role::Planner, Some(1)), at(Role::Worker, Some(1)), at(Role::Qa, Some(1))),
                   ("sonnet".into(), "haiku".into(), qa_bal.clone()), "balanced is the operator's own binding");
        assert_eq!(at(Role::Worker, None), at(Role::Worker, Some(1)), "no dial = balanced");
        // CHEAPER is one rung down for each role, and the bottom rung cannot go lower
        // the dial can still walk QA down if the operator explicitly asks for cheap — but the DEFAULT does not
        assert_eq!((at(Role::Planner, Some(0)), at(Role::Worker, Some(0)), at(Role::Qa, Some(0))),
                   ("haiku".into(), "haiku".into(), if ranked.is_some() { qa_bal.clone() } else { "haiku".into() }), "cheaper walks down, clamped at haiku (a measured gate stays put)");
        // SMARTER is one rung up, and the top rung cannot go higher
        // "smarter" walks QA up to Opus — which the gate bench could not get a verdict out of (6 of 12 silent at a
        // 30-turn cap). The dial is an explicit ask so it is not overridden, but it is a known-bad destination.
        assert_eq!((at(Role::Planner, Some(2)), at(Role::Worker, Some(2)), at(Role::Qa, Some(2))),
                   ("sonnet".into(), "sonnet".into(), if ranked.is_some() { qa_bal.clone() } else { "sonnet".into() }), "smarter walks up, clamped at sonnet: there is no rung above it (never Opus; a measured gate stays put)");
        // every stop is a DIFFERENT run: cheaper is strictly cheaper than smarter for the roles that move
        for r in [Role::Planner, Role::Worker] { assert_ne!(at(r, Some(0)), at(r, Some(2)), "{r:?} must not be the same model at both ends"); }
        // a model the operator typed by hand is anchored at the role's own tier, not dropped
        assert_eq!(shift_model(Engine::ClaudeCode, Some("some-custom-model"), Tier::Mid, -1).as_deref(), Some("haiku"));
        assert_eq!(shift_model(Engine::ClaudeCode, None, Tier::High, 1).as_deref(), Some("sonnet"));
    }
    #[test]
    fn a_worker_is_always_cheaper_than_its_planner_even_across_families() {
        let _env = env_lock();
        // this test measures the routing arithmetic across providers, so the gateway must be reachable for it
        let _zen = ZenOn::on();
        let b = |e: Engine, m: &str| Binding { engine: e, model: Some(m.into()), source: "role" };
        // the ordinary case: nothing to do
        let (w, note) = worker_under_planner(b(Engine::ClaudeCode, "haiku"), &b(Engine::ClaudeCode, "sonnet"));
        assert_eq!((w.model.as_deref(), note), (Some("haiku"), None));
        // the collision: both bound to sonnet — the worker walks down its own ladder
        let (w, note) = worker_under_planner(b(Engine::ClaudeCode, "sonnet"), &b(Engine::ClaudeCode, "sonnet"));
        assert_eq!(w.model.as_deref(), Some("haiku"));
        assert!(note.unwrap().contains("cheaper than its planner"));
        // THE FLOOR: the planner is ALREADY the cheapest Claude model, so the worker must cross families. The test
        // asserts the PROPERTY and not the destination (2026-09-27): which engine is cheapest depends on which seats
        // this machine holds — with Gemini signed in the answer is a flash rung, without it the gateway — and a test
        // that names one engine fails on somebody else's laptop for no defect.
        let (w, note) = worker_under_planner(b(Engine::ClaudeCode, "haiku"), &b(Engine::ClaudeCode, "haiku"));
        assert_ne!(w.engine, Engine::ClaudeCode, "it must leave the family it is already at the bottom of: {note:?}");
        assert!(price_of(w.engine, w.model.as_deref()) < price_of(Engine::ClaudeCode, Some("haiku")), "{note:?}");
        assert!(model_blocked(w.engine, w.model.as_deref()).is_none(), "and whatever it crossed to must be allowed: {w:?}");
        assert_eq!(w.source, "role+cheaper");
        // a cheap worker on another family is left exactly as the operator bound it
        let (w, note) = worker_under_planner(b(Engine::Opencode, "deepseek-v4-flash"), &b(Engine::ClaudeCode, "opus"));
        assert_eq!((w.engine, w.model.as_deref(), note), (Engine::Opencode, Some("deepseek-v4-flash"), None));
        // the prices that rule is built on, across families
        assert!(price_of(Engine::ClaudeCode, Some("opus")) > price_of(Engine::ClaudeCode, Some("sonnet")));
        assert!(price_of(Engine::ClaudeCode, Some("haiku")) > price_of(Engine::Opencode, Some("deepseek-v4-flash")));
        assert!(price_of(Engine::Opencode, Some("deepseek-v4-pro")) < price_of(Engine::ClaudeCode, Some("haiku")));
    }
    #[test]
    fn an_opencode_model_always_reaches_the_wire_with_its_provider() {
        assert_eq!(opencode_model_id("deepseek-v4-flash"), "opencode/deepseek-v4-flash");
        assert_eq!(opencode_model_id("opencode/deepseek-v4-pro"), "opencode/deepseek-v4-pro", "already qualified: untouched");
        assert_eq!(opencode_model_id(" anthropic/claude-sonnet-4 "), "anthropic/claude-sonnet-4", "another provider through OpenCode: kept");
        // the bare name the operator typed becomes the catalogue's id on the command line
        let (_, args) = cli_command(Engine::Opencode, "", &RunOpts { model: Some("deepseek-v4-flash".into()), ..Default::default() });
        assert_eq!(args, vec!["run", "--format", "json", "--model", "opencode/deepseek-v4-flash"]);
    }
    #[test]
    fn every_job_has_a_winner_and_a_fallback_on_every_engine_and_the_bar_moves_them() {
        let _env = env_lock();
        for j in Job::ALL { for e in Engine::all() {
            // GEMINI AND CURSOR HAVE NO MEASURED CELLS YET (2026-09-27) — `("", "")` means "the job decides", the only
            // honest answer for a seat that has never answered a call here. Asserted as an EXPECTATION, not skipped,
            // so that filling them in is what removes this branch.
            if matches!(e, Engine::Gemini | Engine::Cursor) {
                assert_eq!(job_defaults(j, e), ("", ""), "{e:?} has no measured cells yet; fill them from BENCH.md");
                continue;
            }
            let (w, f) = job_defaults(j, e); assert!(!w.is_empty() && !f.is_empty(), "{j:?} on {e:?}");
            let (wb, fb) = job_binding(j, e, None); assert_eq!((wb.model.as_deref(), fb.model.as_deref(), wb.source), (Some(w), Some(f), "measured"));
            if e == Engine::Opencode { assert!(w.contains('/') && f.contains('/'), "OpenCode cells carry their provider: {w} {f}"); }
            if e == Engine::Codex { assert!(["low", "medium", "high"].contains(&w), "Codex cells are efforts: {w}"); }
            // Grok is shaped like Codex and states its cells BY REUSE, so the two can never drift apart
            if e == Engine::Grok { assert_eq!(job_defaults(j, Engine::Grok), job_defaults(j, Engine::Codex), "Grok's cells are Codex's"); }
        } }
        // the measured winners, as the benches of 2026-09-22 left them
        assert_eq!(job_defaults(Job::Lookup, Engine::ClaudeCode).0, "haiku");
        assert_eq!(job_defaults(Job::Synthesis, Engine::ClaudeCode).0, "sonnet");
        assert_eq!(job_defaults(Job::EditorS, Engine::ClaudeCode).0, "sonnet");
        assert_eq!(job_defaults(Job::EditorL, Engine::Opencode).0, "opencode/deepseek-v4-flash");
        // THE BENCHMARK MOVED TWO DEFAULTS (2026-09-23) — the whole point of measuring is that the product changes
        assert_eq!(job_global(Job::QaGate), ((Engine::Opencode, "opencode/qwen3.6-plus"), (Engine::ClaudeCode, "haiku")), "the gate is off Opus: four models tie 4/4 on the graded fixtures, so the cheapest takes it");
        assert!(price_of(Engine::Opencode, Some("opencode/qwen3.6-plus")) < price_of(Engine::ClaudeCode, Some("opus")) / 5.0, "and it is far cheaper, which was the objection to Opus");
        assert_eq!(job_global(Job::Verifier), ((Engine::Opencode, "opencode/qwen3.6-plus"), (Engine::ClaudeCode, "sonnet")), "the verifier follows its audited calls, not the non-discriminating fixtures");
        // the jobs the evidence CONFIRMED rather than changed — a measurement that agrees is still a measurement
        assert_eq!(job_global(Job::EditorL).0, (Engine::Opencode, "opencode/deepseek-v4-flash"));
        assert_eq!(job_global(Job::EditorS).0, (Engine::Opencode, "opencode/qwen3.6-plus"));
        assert_eq!(job_global(Job::Planner).0, (Engine::Opencode, "opencode/deepseek-v4-pro"));
        // THE GATE IS NOT A PRICE DECISION. The tolerance rule ("keep 80 % of the best and the cheaper wins") holds where a
        // failure is recoverable — a writer's slip is caught by the gate. Nothing catches the gate, so a false accept
        // costs unboundedly more than the cent it saves. Sonnet: 12/12, zero false accepts, zero false rejects.
        assert_eq!(job_defaults(Job::QaGate, Engine::ClaudeCode), ("sonnet", "opus"));
        // the bar: CHEAPER walks the winner one rung down its ladder, and the Codex efforts never move (they are not models)
        assert_eq!(job_binding(Job::Synthesis, Engine::ClaudeCode, Some(0)).0.model.as_deref(), Some("haiku"));
        assert_eq!(job_binding(Job::Synthesis, Engine::Codex, Some(0)).0.model.as_deref(), Some("medium"));
        // the operator's cell wins, and clearing it restores the measurement
        set_job_binding("lookup", "claude-code", Some("sonnet"), None).unwrap();
        let (w, f) = job_binding(Job::Lookup, Engine::ClaudeCode, None);
        assert_eq!((w.model.as_deref(), w.source, f.model.as_deref(), f.source), (Some("sonnet"), "job", Some("sonnet"), "measured"));
        set_job_binding("lookup", "claude-code", None, None).unwrap();
        assert_eq!(job_binding(Job::Lookup, Engine::ClaudeCode, None).0.model.as_deref(), Some("haiku"));
        // a role without its own binding is its job's winner
        // (the RAW binding: with Zen off that seat cannot take a call, and `role_binding` itself then returns the seat that can - see `binding_when_unavailable`)
        assert_eq!(role_binding_raw(Role::Worker, Engine::Opencode).model.as_deref(), Some("opencode/deepseek-v4-flash"));
        assert_ne!(role_binding(Role::Worker, Engine::Opencode).engine, Engine::Opencode, "an unreachable seat is never handed out");
        // a Codex effort rides as `-c model_reasoning_effort`, a model name as `-m`
        let (_, a) = cli_command(Engine::Codex, "", &RunOpts { model: Some("high".into()), ..Default::default() });
        assert!(a.windows(2).any(|w| w[0] == "-c" && w[1] == "model_reasoning_effort=\"high\""), "{a:?}");
        let (_, b) = cli_command(Engine::Codex, "", &RunOpts { model: Some("gpt-5-codex".into()), ..Default::default() });
        assert!(b.windows(2).any(|w| w == ["-m", "gpt-5-codex"]), "{b:?}");
    }
    #[test]
    fn the_cheapest_within_three_points_wins_across_providers() {
        let _env = env_lock();
        // this test measures the routing arithmetic across providers, so the gateway must be reachable for it
        let _zen = ZenOn::on();
        // Pro is 1 behind Sonnet as a reader and 10× cheaper → Pro wins lookup; Sonnet is 4 ahead on synthesis → Sonnet
        assert_eq!(job_global(Job::Lookup).0, (Engine::Opencode, "opencode/deepseek-v4-pro"));
        assert_eq!(job_global(Job::Synthesis), ((Engine::ClaudeCode, "sonnet"), (Engine::Opencode, "opencode/qwen3.6-plus")), "two of twenty is five of fifty: outside the band, Sonnet keeps synthesis; Qwen is the fallback");
        // S: Qwen 5/5 at an eighth of Sonnet's price (2026-09-23) → Qwen, Sonnet behind it
        assert_eq!(job_global(Job::EditorS), ((Engine::Opencode, "opencode/qwen3.6-plus"), (Engine::ClaudeCode, "sonnet")));
        // the router's pick is the global winner when its provider is here (always, under test), and never the same model twice
        let (w, f) = job_binding_any(Job::EditorS, Engine::ClaudeCode, None);
        assert_eq!((w.engine, w.model.as_deref(), w.source), (Engine::Opencode, Some("opencode/qwen3.6-plus"), "measured"));
        assert_eq!((f.engine, f.model.as_deref()), (Engine::ClaudeCode, Some("sonnet")));
        // the operator's global cell overrides, "engine:model", and clears back
        set_job_binding("editor-s", "any", Some("claude-code:sonnet"), Some("opencode:opencode/deepseek-v4-flash")).unwrap();
        let (w, f) = job_binding_any(Job::EditorS, Engine::ClaudeCode, None);
        assert_eq!((w.engine, w.model.as_deref(), w.source, f.model.as_deref()), (Engine::ClaudeCode, Some("sonnet"), "job", Some("opencode/deepseek-v4-flash")));
        set_job_binding("editor-s", "any", None, None).unwrap();
        assert_eq!(job_binding_any(Job::EditorS, Engine::ClaudeCode, None).0.engine, Engine::Opencode);
        // every job has a global winner and a fallback on a different model
        for j in Job::ALL { let (w, f) = job_global(j); assert_ne!(w, f, "{j:?}"); }
    }
    #[test]
    fn a_pair_gives_the_deciding_jobs_to_the_brain_and_the_producing_jobs_to_the_hands() {
        let _env = env_lock();
        // PORTED OFF THE ZEN PAIRS (2026-09-27). The property is unchanged — the brain takes the deciding jobs and the
        // hands the producing ones — but it is asserted on subscription merges, because the metered ones were withdrawn.
        // `claude-gemini` has an unnamed hands side, so it also covers "a side whose model the job picks says so".
        let cg = Pair::builtin("gemini-composer").unwrap();
        assert_eq!(cg.label(), "Antigravity · the job's model × Cursor · Composer 2.5", "a side whose model the job picks says so");
        // the brain's own cells decide the deciding jobs; the hands engine takes the producing ones
        assert_eq!(job_binding_paired(Job::Planner, Some(&cg), Engine::Cursor, None).0.engine, Engine::Gemini, "planning is the brain's");
        assert_eq!(job_binding_paired(Job::QaGate, Some(&cg), Engine::Cursor, None).0.engine, Engine::Gemini, "judging is the brain's");
        assert_eq!(job_binding_paired(Job::EditorL, Some(&cg), Engine::ClaudeCode, None).0.engine, Engine::Cursor, "writing is the hands'");
        assert_eq!(job_binding_paired(Job::Lookup, Some(&cg), Engine::ClaudeCode, None).0.engine, Engine::Cursor, "looking up is the hands'");
        // a pair that NAMES the hands' model: it does every hands job, and the cell's winner becomes the fallback
        let cc = Pair::builtin("claude-composer").unwrap();
        assert_eq!(cc.label(), "Claude · Sonnet × Cursor · Composer 2.5");
        let (w, _f) = job_binding_paired(Job::EditorL, Some(&cc), Engine::ClaudeCode, None);
        assert_eq!((w.engine, w.model.as_deref(), w.source), (Engine::Cursor, Some("composer-2.5"), "pair"));
        assert_eq!(job_binding_paired(Job::Verifier, Some(&cc), Engine::ClaudeCode, None).0.model.as_deref(), Some("sonnet"), "the brain is untouched");
        // PROVIDER + MODEL, ALWAYS BOTH (operator 2026-09-23: "never just provider, never just model")
        assert_eq!(side_name(Engine::ClaudeCode, Some("sonnet")), "Claude · Sonnet");
        assert_eq!(side_name(Engine::Opencode, Some("opencode/deepseek-v4-flash")), "OpenCode · DeepSeek v4 Flash");
        assert_eq!(side_name(Engine::Opencode, Some("opencode/qwen3.6-plus")), "OpenCode · Qwen 3.6 Plus");
        assert_eq!(side_name(Engine::Codex, Some("gpt-5.6-terra@high")), "Codex gpt-5.6-terra · high");
        assert_eq!(side_name(Engine::Codex, None), "Codex · the job's model", "a side with no model says so instead of naming a provider alone");
        assert_eq!(side_name(Engine::Codex, Some("high")), "Codex · the account's model · high", "a bare effort is an effort, not a model name");
        assert!(!side_name(Engine::Opencode, Some("opencode/kimi-k2.7-code")).starts_with("Kimi"), "a model is never named without its provider");
        // a Codex brain that names a model KEEPS it — the bench of terra · sol · luna depends on this
        let cx = Pair { name: "codex-flash".into(), brain: Engine::Codex, hands: Engine::Opencode, brain_model: Some("gpt-5.6-sol@high".into()), hands_model: Some("opencode/deepseek-v4-flash".into()) };
        assert_eq!(job_binding_paired(Job::Planner, Some(&cx), Engine::ClaudeCode, None).0, Binding { engine: Engine::Codex, model: Some("gpt-5.6-sol@high".into()), source: "pair" });
        assert_eq!(job_binding_paired(Job::EditorL, Some(&cx), Engine::ClaudeCode, None).0.model.as_deref(), Some("opencode/deepseek-v4-flash"));
        assert_eq!(job_binding_paired(Job::Planner, Some(&cx), Engine::ClaudeCode, Some(0)).0.model.as_deref(), Some("gpt-5.6-sol@high"), "the cost dial never rewrites a Codex model");
        // CODEX IS A PROVIDER (operator 2026-09-23): its models are named, and each runs at an effort
        assert_eq!(codex_model_effort("gpt-5.6-sol@high"), (Some("gpt-5.6-sol".into()), Some("high".into())));
        assert_eq!(codex_model_effort("gpt-5.6-terra:low"), (Some("gpt-5.6-terra".into()), Some("low".into())));
        assert_eq!(codex_model_effort("medium"), (None, Some("medium".into())), "a bare effort keeps the account's model");
        assert_eq!(codex_model_effort("gpt-5.6-luna"), (Some("gpt-5.6-luna".into()), None), "a bare name is a model");
        assert_eq!(codex_model_effort("gpt-7-whatever@daft"), (Some("gpt-7-whatever".into()), None), "an unknown effort is dropped, the model stands");
        assert_eq!(codex_model_effort(""), (None, None));
        // NEVER TWO FRONTIER MODELS (operator 2026-09-23): a merge is a frontier brain with cheap hands, or two cheap
        // models — two frontier sides buys both prices and the slower speed, which is the opposite of the point.
        assert!(Pair::is_frontier(Engine::Codex, None) && Pair::is_frontier(Engine::ClaudeCode, Some("opus")) && Pair::is_frontier(Engine::ClaudeCode, Some("sonnet")));
        assert!(!Pair::is_frontier(Engine::ClaudeCode, Some("haiku")), "haiku is the cheap side of Claude");
        assert!(!Pair::is_frontier(Engine::Opencode, Some("opencode/deepseek-v4-flash")) && !Pair::is_frontier(Engine::Opencode, Some("opencode/qwen3.6-plus")));
        assert!(Pair::is_frontier(Engine::Opencode, Some("opencode/gpt-5.6-terra")), "the gateway can serve a frontier model too");
        // CODEX AT LOW EFFORT IS THE CHEAP SIDE, which is what lets it be the hands of a merge
        assert!(!Pair::is_frontier(Engine::Codex, Some("low")), "low effort is Codex's cheap rung");
        assert!(Pair::is_frontier(Engine::Codex, Some("medium")) && Pair::is_frontier(Engine::Codex, Some("high")));
        for name in Pair::BUILTINS { assert_eq!(Pair::builtin(name).unwrap().problem(), None, "the built-in {name} breaks the rule"); }
        let both = Pair { name: "two-frontier".into(), brain: Engine::ClaudeCode, hands: Engine::Codex,
                          brain_model: Some("opus".into()), hands_model: Some("high".into()) };
        assert!(both.problem().unwrap().contains("both frontier"), "{:?}", both.problem());
        assert_eq!(Pair { hands_model: Some("low".into()), ..both.clone() }.problem(), None, "frontier brain + cheap hands is the shape");
        // THE ZEN MERGES ARE WITHDRAWN, NOT RENAMED (operator 2026-09-27: "remove all zen merges completely"). Every
        // retired name must resolve to NOTHING — a merge that cannot be named cannot be launched, which is the point.
        for retired in Pair::LEGACY_BUILTINS {
            assert!(Pair::builtin(retired).is_none(), "{retired} is discontinued and must not resolve");
            assert!(Pair::is_legacy_name(retired));
        }
        for alias in ["claudeseek", "claude-kimi", "claude × deepseek", "deepseek"] {
            assert!(Pair::builtin(alias).is_none(), "{alias} is an alias of a withdrawn merge");
        }
        // EVERY BUILT-IN IS A SUBSCRIPTION ON BOTH SIDES — the moat. No Zen, no Cursor third-party model.
        for name in Pair::BUILTINS {
            let p = Pair::builtin(name).unwrap();
            for (e, m) in [(p.brain, p.brain_model.as_deref()), (p.hands, p.hands_model.as_deref())] {
                assert!(!e.is_metered_gateway(), "{name} reaches a metered gateway");
                assert!(model_blocked(e, m).is_none(), "{name}: {:?}", model_blocked(e, m));
                let side = format!("{}:{}", e.key(), m.unwrap_or(""));
                assert!(side_is_metered(&side).is_none(), "{name} side {side} is metered: {:?}", side_is_metered(&side));
            }
        }
        // solo = the global rule. It used to land on OpenCode, because the gateway's bottom rung was the cheapest
        // thing in the table; with Zen withdrawn a solo run stays on the engine it was started with (2026-09-27).
        assert_eq!(job_binding_paired(Job::EditorS, None, Engine::ClaudeCode, None).0.engine, Engine::ClaudeCode);
        // the pane's setting: a name, a full shape, or solo
        assert_eq!(Pair::of_settings(&json!({ "pair": "claude-grok" })).unwrap().name, "claude-grok");
        assert_eq!(Pair::of_settings(&json!({ "pair": "solo" })), None);
        assert_eq!(Pair::of_settings(&json!({})), None);
        let custom = Pair::of_settings(&json!({ "pair": { "name": "mine", "brain": { "engine": "codex", "model": "high" }, "hands": { "engine": "opencode", "model": "opencode/glm-5.3" } } })).unwrap();
        assert_eq!((custom.label(), custom.brain, custom.hands_model.as_deref()), ("Codex · the account's model · high × OpenCode · GLM 5.3".into(), Engine::Codex, Some("opencode/glm-5.3")));
        assert_eq!(job_binding_paired(Job::Planner, Some(&custom), Engine::ClaudeCode, None).0, Binding { engine: Engine::Codex, model: Some("high".into()), source: "pair" });
    }
    #[test]
    fn an_ask_is_classified_without_a_model_call() {
        assert_eq!(classify_ask("Where does a React element's `key` warning originate, and which module raises it?"), Job::Lookup);
        assert_eq!(classify_ask("Which packages depend on shared/ReactSharedInternals?"), Job::Lookup);
        assert_eq!(classify_ask("Why does the commit phase never throw, and how is render different?"), Job::Synthesis);
        assert_eq!(classify_ask("If I wanted to add a new hook, which files must change and in which order?"), Job::Synthesis);
        assert_eq!(classify_ask("Explain the scheduler's expiry rule"), Job::Synthesis);
        assert_eq!(classify_ask("In src/products.ts, change `formatPrice` to use Intl.NumberFormat and add a JSDoc comment. `npm run build` must pass."), Job::EditorS);
        assert_eq!(classify_ask("Add eight modules under src/features/: search.ts, sort.ts, wishlist.ts, cart-storage.ts, money.ts, quantity.tsx, checkout-form.ts, recently-viewed.ts — each fully typed."), Job::EditorL);
        assert_eq!(classify_ask(&"Build an ORDERS feature. ".repeat(30)), Job::EditorL, "a long ask is never S");
    }
    #[test]
    fn every_call_is_priced_deterministically_from_its_model_and_tokens() {
        // Sonnet: 100k in, 10k out, 1M cache reads, 50k cache writes → 0.30 + 0.15 + 0.30 + 0.1875
        let c = price_tokens(Engine::ClaudeCode, Some("sonnet"), 100_000, 10_000, 1_000_000, 50_000);
        assert!((c - 0.9375).abs() < 1e-9, "{c}");
        assert!((price_tokens(Engine::Opencode, Some("opencode/deepseek-v4-flash"), 1_000_000, 100_000, 0, 0) - 0.168).abs() < 1e-9);
        assert!(price_tokens(Engine::ClaudeCode, Some("opus"), 1, 1, 0, 0) > price_tokens(Engine::ClaudeCode, Some("haiku"), 1, 1, 0, 0));
        assert_eq!(prices_of(Engine::Codex, Some("high")), (1.25, 10.0), "a Codex effort prices as the account's model");
        assert_eq!(price_tokens(Engine::ClaudeCode, None, 0, 0, 0, 0), 0.0);
    }
    #[test]
    fn fold_and_command_shape() {
        let (sys, p) = fold_messages(&[m("system", "be brief"), m("user", "hi")], true);
        assert_eq!(sys, "be brief"); assert!(p.starts_with("hi\n\nRespond with a single JSON object"));
        let (_, p2) = fold_messages(&[m("user", "a"), m("assistant", "b"), m("user", "c")], false);
        assert_eq!(p2, "USER:\na\n\nASSISTANT:\nb\n\nUSER:\nc");
        let (bin, args) = cli_command(Engine::ClaudeCode, "sys", &RunOpts { write: true, ..Default::default() });
        assert_eq!(bin, "claude"); assert!(args.contains(&"--append-system-prompt".to_string()) && args.contains(&"Read,Grep,Glob,Edit,Write,MultiEdit,Bash".to_string()) && args.contains(&"30".to_string()));
        let (bin, args) = cli_command(Engine::Codex, "", &RunOpts { last_message_file: Some("/tmp/x.md".into()), ..Default::default() });
        assert_eq!(bin, "codex"); assert_eq!(args, ["exec", "--json", "--skip-git-repo-check", "--sandbox", "read-only", "--output-last-message", "/tmp/x.md", "-"]);
        // NO TIER, STILL A MODEL (2026-09-22): a call without a tier used to omit `--model` and OpenCode billed the
        // operator's account default (a GPT-5-nano); the mid-tier model is named instead
        assert_eq!(cli_command(Engine::Opencode, "", &RunOpts::default()).1, vec!["run", "--format", "json"]);   // no model of ours: OpenCode runs on the person's own default (2026-11-26)
        assert_eq!(cli_command(Engine::Opencode, "", &RunOpts { tier: Some(Tier::Low), ..Default::default() }).1, vec!["run", "--format", "json"]);
        assert_eq!(engine_of(&json!("cli:claude")), Some(Engine::ClaudeCode)); assert_eq!(engine_of(&json!("opencode-direct")), Some(Engine::Opencode)); assert_eq!(engine_of(&json!("gpt-4")), None);
    }

    // THE TIERS (2026-09-19): a tier is the engine's model/effort switch; no tier → no flag; env overrides a cell
    #[test]
    fn a_role_binds_to_an_engine_and_a_model_the_operator_named() {
        // unconfigured: a role falls back to the tier it always ran at, on the run's own engine
        let b = role_binding(Role::Worker, Engine::ClaudeCode);
        assert_eq!((b.engine, b.model.as_deref(), b.source), (Engine::ClaudeCode, Some("haiku"), "measured"), "a role without its own binding is its job's measured winner");
        assert_eq!(role_binding(Role::Planner, Engine::ClaudeCode).model.as_deref(), Some("sonnet"));
        // THE GATEKEEPER (operator 2026-10-07: "Heimdall should route always. Deterministically."): an unpinned QA is the first usable seat of the measured ranking (`qa_gate_ranking`), whichever seats this machine
        // has; with none usable the old tier rule answers (sonnet). Chosen on false accepts alone - it is the last line, so a false accept is unrecoverable.
        let qa = role_binding(Role::Qa, Engine::ClaudeCode);
        match qa_gate_pick(None, seat_ok) {
            Some(g) => assert_eq!((qa.engine, qa.model.as_deref(), qa.source), (g.engine, g.model.as_deref(), "qa-gate-ranking")),
            None => assert_eq!(qa.model.as_deref(), Some("sonnet"), "nothing in the ranking is usable here: the tier rule answers"),
        }
        assert_eq!(Role::of("PLANNER"), Some(Role::Planner)); assert_eq!(Role::of("judge"), None);
        // the model rides on RunOpts and wins over the tier
        let (_, a) = cli_command(Engine::ClaudeCode, "", &RunOpts { tier: Some(Tier::High), model: Some("haiku".into()), ..Default::default() });
        assert!(a.windows(2).any(|w| w == ["--model", "haiku"]), "the role's model wins: {a:?}");
    }
    #[test]
    fn tiers_become_model_flags_per_engine() {
        let _env = env_lock();
        assert_eq!(Tier::of("HIGH"), Some(Tier::High)); assert_eq!(Tier::of("medium"), Some(Tier::Mid)); assert_eq!(Tier::of("x"), None); assert_eq!(Tier::Low.up(), Tier::Mid); assert_eq!(Tier::High.up(), Tier::High);
        assert!(Tier::Low < Tier::High);
        let (_, a) = cli_command(Engine::ClaudeCode, "", &RunOpts { tier: Some(Tier::High), ..Default::default() });
        assert!(a.windows(2).any(|w| w == ["--model", "sonnet"]), "the High tier is Sonnet (never Opus): {a:?}");
        let (_, a) = cli_command(Engine::ClaudeCode, "", &RunOpts { tier: Some(Tier::Low), ..Default::default() });
        assert!(a.windows(2).any(|w| w == ["--model", "haiku"]), "low is haiku since the bench of 2026-09-21");
        let (_, a) = cli_command(Engine::ClaudeCode, "", &RunOpts { tier: Some(Tier::Mid), ..Default::default() });
        assert!(a.windows(2).any(|w| w == ["--model", "sonnet"]));
        let (_, a) = cli_command(Engine::Codex, "", &RunOpts { tier: Some(Tier::Low), ..Default::default() });
        assert!(!a.contains(&"-m".to_string()) && a.windows(2).any(|w| w == ["-c", "model_reasoning_effort=\"low\""]), "codex low = low effort on the account's model: {a:?}");
        let (_, a) = cli_command(Engine::Codex, "", &RunOpts { tier: Some(Tier::High), ..Default::default() });
        assert!(!a.contains(&"-m".to_string()) && a.windows(2).any(|w| w == ["-c", "model_reasoning_effort=\"high\""]), "{a:?}");
        assert_eq!(cli_command(Engine::Opencode, "", &RunOpts { tier: Some(Tier::High), ..Default::default() }).1, vec!["run", "--format", "json"]);
        unsafe { std::env::set_var("ANTILOKI_TIER_MODEL_OPENCODE_HIGH", "anthropic/claude-opus-5"); }
        assert_eq!(cli_command(Engine::Opencode, "", &RunOpts { tier: Some(Tier::High), ..Default::default() }).1, vec!["run".to_string(), "--format".to_string(), "json".to_string(), "--model".to_string(), "anthropic/claude-opus-5".to_string()]);
        unsafe { std::env::set_var("ANTILOKI_TIER_MODEL_CLAUDE_CODE_HIGH", "default"); }
        assert!(!cli_command(Engine::ClaudeCode, "", &RunOpts { tier: Some(Tier::High), ..Default::default() }).1.contains(&"--model".to_string()));
        unsafe { std::env::remove_var("ANTILOKI_TIER_MODEL_OPENCODE_HIGH"); std::env::remove_var("ANTILOKI_TIER_MODEL_CLAUDE_CODE_HIGH"); }
    }

    #[test]
    fn parse_claude_stream_json() {
        let out = "mise ~/.config/mise/config.toml tools: node@24\n{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{\"file_path\":\"src/a.ts\"}}]}}\n{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"done\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":90,\"output_tokens\":5},\"total_cost_usd\":0.01}";
        let p = parse_cli_output(Engine::ClaudeCode, out, None);
        assert_eq!(p.content, "done"); assert_eq!(p.tokens_in, Some(19), "10 input + 90 cache reads at a tenth — priced, not summed"); assert_eq!(p.tokens_out, Some(5)); assert_eq!(p.cost, Some(0.01));
        assert_eq!(p.tools.len(), 1); assert_eq!(p.tools[0].target, "src/a.ts");
        let e = parse_cli_output(Engine::ClaudeCode, "{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"is_error\":true,\"result\":\"too long\"}", None);
        assert_eq!(e.content, ""); assert_eq!(e.error.as_deref(), Some("max turns: too long"));
        let plain = parse_cli_output(Engine::ClaudeCode, "just text", None);
        assert_eq!(plain.content, "just text");
    }

    #[test]
    fn parse_opencode_json_stream() {
        let out = ["{\"type\":\"tool_use\",\"part\":{\"type\":\"tool\",\"tool\":\"read\",\"state\":{\"input\":{\"filePath\":\"src/a.ts\"}}}}",
            "{\"type\":\"step_finish\",\"part\":{\"type\":\"step-finish\",\"tokens\":{\"input\":1000,\"output\":100,\"reasoning\":0,\"cache\":{\"write\":0,\"read\":10000}},\"cost\":0.0012}}",
            "{\"type\":\"text\",\"part\":{\"type\":\"text\",\"text\":\"done: \"}}", "{\"type\":\"text\",\"part\":{\"type\":\"text\",\"text\":\"src/a.ts written\"}}",
            "{\"type\":\"step_finish\",\"part\":{\"type\":\"step-finish\",\"tokens\":{\"input\":500,\"output\":50,\"reasoning\":10,\"cache\":{\"write\":200,\"read\":0}},\"cost\":0.0003}}"].join("\n");
        let p = parse_cli_output(Engine::Opencode, &out, None);
        assert_eq!(p.content, "done: src/a.ts written"); assert_eq!(p.tools.len(), 1); assert_eq!(p.tools[0].target, "src/a.ts");
        assert_eq!(p.tokens_in, Some(1000 + 1000 + 500 + 250), "input + cache reads at a tenth + cache writes at 1.25"); assert_eq!(p.tokens_out, Some(160));
        assert!((p.cost.unwrap() - 0.0015).abs() < 1e-9, "OpenCode's own cost, summed");
    }
    #[test]
    fn parse_codex_and_opencode() {
        let out = "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"command\":\"/usr/bin/bash -lc 'ls src'\"}}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"the answer\"}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}";
        let p = parse_cli_output(Engine::Codex, out, None);
        assert_eq!(p.content, "the answer"); assert_eq!(p.tools[0].target, "ls src"); assert_eq!(p.tokens_in, Some(7)); assert_eq!(p.tokens_out, Some(3));
        assert_eq!(parse_cli_output(Engine::Codex, out, Some("from file")).content, "from file");
        // a stream that ends in an error and no message: the error is the answer, the raw events never leak as content
        let limited = "{\"type\":\"thread.started\",\"thread_id\":\"x\"}\n{\"type\":\"turn.started\"}\n{\"type\":\"error\",\"message\":\"You've hit your usage limit. Upgrade to Plus\"}\n{\"type\":\"turn.failed\",\"error\":{\"message\":\"You've hit your usage limit. Upgrade to Plus\"}}\n";
        let l = parse_cli_output(Engine::Codex, limited, None);
        assert_eq!(l.content, ""); assert_eq!(l.error.as_deref(), Some("You've hit your usage limit. Upgrade to Plus"));
        assert!(parse_cli_output(Engine::Codex, "codex\nlegacy text answer\ntokens used\n12\n", None).content.contains("legacy text answer"), "the text format still parses");
        let oc = parse_cli_output(Engine::Opencode, "> build · gpt\n\x1b[32mhello\x1b[0m world\n", None);
        let tools = opencode_tools("\x1b[0m→ \x1b[0mRead packages/a/b.js\n✱ Grep \"typeOf\" in . · 5 matches\n$ git status --short\n✗ npx prettier failed\nError: nope\n");
        assert_eq!(tools.iter().map(|t| format!("{}:{}", t.name, t.target)).collect::<Vec<_>>(), vec!["Read:packages/a/b.js", "Grep:\"typeOf\" in . · 5 matches", "Bash:git status --short"]);
        assert_eq!(oc.content, "hello world");
    }

    #[test]
    fn completion_shape_and_traces() {
        let p = CliParsed { content: "x".into(), tokens_in: Some(2), tokens_out: Some(3), cost: Some(0.5), ..Default::default() };
        let c = completion("cli:codex", "x", &p);
        assert_eq!(c["choices"][0]["message"]["content"], "x"); assert_eq!(c["usage"]["total_tokens"], 5); assert_eq!(c["usage"]["cost"], 0.5);
        let t = record_trace(CliTrace { model: None, id: 0, engine: Engine::Codex, cwd: "/".into(), started: 1, ended: 2, ms: 1, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 1, ok: true, error: None });
        assert!(t.id >= 1); assert!(traces_since(0).iter().any(|x| x.id == t.id)); assert!(traces_since(3).iter().all(|x| x.ended >= 3));
    }
    /// ZEN IS OFF UNTIL THE OPERATOR TURNS IT ON (operator 2026-09-27: "I don't have Zen credit … don't use things
    /// that have Zen. Make sure that deterministically we're not gonna have this error anymore").
    ///
    /// The bar is DETERMINISM, not care: twelve bench runs were spent on HTTP 402 and six of them entered the record
    /// as quality 0.2 because every layer assumed some other layer had checked. So the refusal is asserted at the two
    /// places that matter — the router's chokepoint (`engine_usable`) and the last gate before an argv exists
    /// (`model_blocked`, which `run_cli_inner` calls again).
    #[test]
    fn zen_is_unreachable_until_the_operator_turns_it_on() {
        let _env = env_lock();
        assert!(!zen_enabled(), "a fresh install must not reach a metered gateway");
        assert!(model_blocked(Engine::Opencode, None).is_some(), "OpenCode must be refused while Zen is off");
        assert!(!engine_usable(Engine::Opencode), "the router must not offer it either");
        // and the other seats are untouched by that switch — this must not become "nothing works"
        assert!(model_blocked(Engine::ClaudeCode, Some("sonnet")).is_none());
        assert!(model_blocked(Engine::Gemini, None).is_none());

        for m in ["opencode-go/claude-haiku-5-5", "ollama/moondream:latest"] { assert!(model_blocked(Engine::Opencode, Some(m)).is_none(), "{m} does not go through Zen"); assert!(member_usable(Engine::Opencode, Some(m))); }
        for m in ["opencode/big-pickle", "claude-haiku-5-5", "opencode-go/", "openai/gpt-5.5", "antiloki-proxy/openrouter/auto"] { assert!(model_blocked(Engine::Opencode, Some(m)).is_some() && !member_usable(Engine::Opencode, Some(m)), "{m} may bill by use"); }
        assert!(model_blocked(Engine::Opencode, Some("opencode-go/deepseek-v4-pro")).is_some(), "the Chinese-models switch still holds on the Go plan");
        set_provider_flag("zen", true).expect("the switch is writable");
        assert!(zen_enabled());
        assert!(model_blocked(Engine::Opencode, None).is_none(), "turned on, it is reachable again");
        set_provider_flag("zen", false).expect("and back off");
        assert!(model_blocked(Engine::Opencode, None).is_some());
    }

    /// CHINESE MODELS ARE A SEPARATE SWITCH, AND A FAMILY NOT A GATEWAY (operator 2026-09-27: "a button on settings to
    /// activate or deactivate Chinese models. Don't make Chinese models obligatory").
    ///
    /// Two switches and not one because they answer different questions: Zen is a BILLING PATH, this is a MODEL
    /// FAMILY — so it must still hold when the same model arrives through a direct key or another gateway, which is
    /// why the test asserts it on an engine that is not OpenCode.
    #[test]
    fn opencode_behind_the_wall_never_uses_a_service_outside_it() {
        let walled = RunOpts { wall: Some(Default::default()), write: true, model: Some("opencode-go/x".into()), ..Default::default() };
        let (_, a) = cli_command(Engine::Opencode, "", &walled); assert!(a.contains(&"--standalone".to_string()) && a.contains(&"--auto".to_string()), "{a:?}");
        let (_, a) = cli_command(Engine::Opencode, "", &RunOpts { wall: Some(Default::default()), ..Default::default() }); assert!(a.contains(&"--standalone".to_string()) && !a.contains(&"--auto".to_string()));
        let (_, a) = cli_command(Engine::Opencode, "", &RunOpts::default()); assert!(!a.contains(&"--standalone".to_string()));
    }
    #[test]
    fn chinese_models_are_off_by_default_whichever_engine_serves_them() {
        let _env = env_lock();
        assert!(!chinese_models_enabled());
        for m in ["qwen3.6-plus", "opencode/deepseek-v4-flash", "kimi-k2.7-code", "glm-5.3", "MiniMax-abab7"] {
            assert!(is_chinese_model(m), "{m} is a Chinese model");
            // asserted on ClaudeCode — an engine with no own-model guard of its own — because the point is that the
            // switch is about the FAMILY and not about how the model is reached
            assert!(model_blocked(Engine::ClaudeCode, Some(m)).is_some(), "{m} must be refused while the switch is off");
        }
        for m in ["sonnet", "opus", "gpt-5.6-terra", "grok-code-fast", "composer-2.5"] {
            assert!(!is_chinese_model(m), "{m} is not a Chinese model");
            assert!(model_blocked(Engine::ClaudeCode, Some(m)).is_none(), "{m} must not be caught by that switch");
        }
        set_provider_flag("chinese_models", true).expect("writable");
        assert!(model_blocked(Engine::ClaudeCode, Some("qwen3.6-plus")).is_none(), "turned on, it is allowed");
        set_provider_flag("chinese_models", false).expect("and back off");
        // the two switches are independent: Chinese on does NOT open the gateway
        set_provider_flag("chinese_models", true).expect("writable");
        assert!(model_blocked(Engine::Opencode, None).is_some(), "Zen stays shut when only the family switch is on");
        set_provider_flag("chinese_models", false).expect("clean up");
    }

    /// THE CHEAPEST RUNG IS NOT AN EXCUSE TO CROSS TO A DISABLED PROVIDER (2026-09-27). `worker_under_planner` named
    /// three engines unconditionally, so a worker could be pushed onto OpenCode purely for being cheap — which is
    /// exactly how a provider that is "off" leaks back into a run.
    #[test]
    fn the_cross_family_fallback_never_lands_on_a_provider_that_is_off() {
        let _env = env_lock();
        let planner = Binding { engine: Engine::ClaudeCode, model: Some("opus".into()), source: "test" };
        let (w, _note) = worker_under_planner(Binding { engine: Engine::Opencode, model: Some("opencode/deepseek-v4-pro".into()), source: "test" }, &planner);
        assert_ne!(w.engine, Engine::Opencode, "Zen is off: the worker must not be left on it — got {w:?}");
        assert!(model_blocked(w.engine, w.model.as_deref()).is_none(), "and whatever it chose must itself be allowed: {w:?}");
    }

    /// EACH NEW SEAT IS REACHED THE WAY ITS OWN CLI WANTS TO BE (all three probed 2026-09-27).
    /// Gemini reads the prompt from stdin; Grok cannot (`os error 6` — it opens a tty) and needs `--prompt-file`;
    /// the prompt never lands in argv for either, which is this module's standing rule.
    #[test]
    fn the_new_engines_are_invoked_the_way_their_clis_documented() {
        let _env = env_lock();
        assert_eq!(Engine::Cursor.bin(), "cursor-agent", "`cursor` is the editor; the agent is cursor-agent");
        // THE CREDENTIAL PATHS ARE FACTS, NOT GUESSES (2026-09-27). Cursor's seat was reported absent on a machine with
        // a paid, working seat because both paths in this arm were invented. Every engine's seat must name a file that
        // actually holds a credential — a config file does not count, and a guess fails CLOSED, silently withholding a
        // provider the operator pays for. Asserted as the strings, since that is the part that was wrong.
        for e in Engine::all() { assert!(engine_seat(e).is_some(), "{} has no seat check", e.key()); }
        assert!(!Engine::Gemini.needs_prompt_file() && Engine::Grok.needs_prompt_file());
        assert!(Engine::Opencode.is_metered_gateway() && !Engine::Gemini.is_metered_gateway());

        // GEMINI IS REACHED BY ANTIGRAVITY NOW (researched 2026-09-27): Google stopped serving Gemini CLI for
        // Pro/Ultra/free on 18 June 2026, so `agy` is preferred and the legacy binary is the API-key fallback.
        assert_eq!(Engine::Gemini.bins(), &["agy", "gemini"], "agy first — the legacy CLI is metered API keys only");
        assert_eq!(Engine::Gemini.bin(), "agy");
        let (bin, a) = cli_command(Engine::Gemini, "", &RunOpts::default());
        assert_eq!(bin, "agy");
        assert!(a.windows(2).any(|w| w == ["--output-format", "stream-json"]), "{a:?}");
        // (Gemini is the exception to the stdin rule — see the prompt_argv assertions below and the note in cli_command)
        // THIS ASSERTION USED TO READ "a read-only run must not self-approve", and measurement overturned it
        // (2026-09-27). Without the flag Antigravity soft-denies the tool it needs and returns `status: SUCCESS` with an
        // EMPTY response and exit 0 — so "do not self-approve" bought no safety and cost every answer. What keeps a
        // read-only run read-only is `--mode plan`, which cannot edit; the flag only removes a prompt headless mode is
        // structurally unable to answer. The safety lives in the mode, not in withholding the flag.
        assert!(a.windows(2).any(|w| w == ["--mode", "plan"]), "read-only is plan mode: {a:?}");
        assert!(a.iter().any(|x| x == "--dangerously-skip-permissions"), "and it must still be able to answer: {a:?}");
        let (_, aw) = cli_command(Engine::Gemini, "", &RunOpts { write: true, ..Default::default() });
        assert!(aw.iter().any(|x| x == "--dangerously-skip-permissions"), "headless has no prompt to answer: {aw:?}");
        assert!(!aw.iter().any(|x| x == "plan"), "a write run is not plan mode: {aw:?}");

        // CURSOR, VERIFIED BY A REAL CALL — and it stalls in a pipe without --trust ("Workspace Trust Required")
        let (bin, c) = cli_command(Engine::Cursor, "", &RunOpts::default());
        assert_eq!(bin, "cursor-agent");
        assert!(c.iter().any(|x| x == "--trust"), "a headless run cannot answer a trust prompt: {c:?}");
        assert!(c.windows(2).any(|w| w == ["--mode", "plan"]), "read-only is plan mode: {c:?}");
        let (_, cw) = cli_command(Engine::Cursor, "", &RunOpts { write: true, ..Default::default() });
        assert!(cw.iter().any(|x| x == "--force") && !cw.iter().any(|x| x == "plan"), "{cw:?}");
        // Cursor is used only for what it ADDS: its own Composer and its Grok, never the Claude/GPT models the
        // operator already holds on their own seats.
        assert_eq!(tier_model(Engine::Cursor, Tier::Low).as_deref(), Some("composer-2.5"));
        assert_eq!(tier_model(Engine::Cursor, Tier::Mid).as_deref(), Some("cursor-grok-4.6-medium"));
        for t in [Tier::Low, Tier::Mid, Tier::High] {
            let m = tier_model(Engine::Cursor, t).unwrap_or_default();
            // EVERY RUNG MUST SIT IN CURSOR'S OWN POOL, not the $20/month third-party allowance. Cursor's own are
            // Composer and the `cursor-grok-*` builds; a bare `grok-4.7-*`, or any Claude/GPT/Gemini, bills at API
            // rates against the pool that empties.
            assert!(m.starts_with("composer") || m.starts_with("cursor-grok"),
                    "{m} is outside Cursor's own pool — it bills at API rates against the $20 allowance");
            assert!(!m.ends_with("-fast"), "{m}: priority compute costs more for the same answer");
            for held in ["claude", "gpt", "codex", "gemini"] {
                assert!(!m.contains(held), "reaching {m} through Cursor spends the draining pool on a seat already held");
            }
        }
        // Cursor's own pool carries no token rate, so everything in it is the cheap side of a pair
        assert!(!Pair::is_frontier(Engine::Cursor, Some("composer-2.5")));
        assert!(!Pair::is_frontier(Engine::Cursor, Some("cursor-grok-4.5-high")), "its own Grok build is in that pool too");
        assert!(Pair::is_frontier(Engine::Cursor, Some("claude-opus-5-thinking-high")), "a third-party model bills API rates");

        // "NEVER THE THIRD PARTY" IS A GUARD, NOT A DEFAULT (operator 2026-09-27) — naming one by hand is refused.
        for own in ["composer-2.5", "cursor-grok-4.6-high", "cursor-grok-4.5-medium"] {
            assert!(is_cursor_own_model(own));
            assert!(model_blocked(Engine::Cursor, Some(own)).is_none(), "{own} is Cursor's own");
        }
        for third in ["claude-opus-5-thinking-high", "gpt-5.3-codex", "gemini-3.7-flash-high", "grok-4.7-high", "auto"] {
            assert!(!is_cursor_own_model(third), "{third} is not in Cursor's own pool");
            assert!(model_blocked(Engine::Cursor, Some(third)).is_some(),
                    "{third} bills against the $20 allowance and must be refused on Cursor");
        }
        // ANTIGRAVITY PROXIES OTHER VENDORS TOO (`agy models` lists claude-sonnet-4-6 and gpt-oss-120b-medium), so the
        // same rule holds: a Gemini allowance is for Google's models, not for a seat the operator already holds.
        for own in ["gemini-3.8-flash-low", "gemini-3.8-flash-medium", "gemini-3.1-pro-high"] {
            assert!(is_gemini_own_model(own));
            assert!(model_blocked(Engine::Gemini, Some(own)).is_none(), "{own} is Google's own");
        }
        for third in ["claude-sonnet-4-6", "claude-opus-4-6-thinking", "gpt-oss-120b-medium"] {
            assert!(!is_gemini_own_model(third));
            assert!(model_blocked(Engine::Gemini, Some(third)).is_some(),
                    "{third} through Antigravity spends a Gemini allowance on another vendor");
        }
        // the ladder is the rungs the seat actually offers, cheap flash first
        assert_eq!(tier_model(Engine::Gemini, Tier::Low).as_deref(), Some("gemini-3.8-flash-low"));
        assert_eq!(tier_model(Engine::Gemini, Tier::High).as_deref(), Some("gemini-3.1-pro-high"));
        for t in [Tier::Low, Tier::Mid, Tier::High] {
            let m = tier_model(Engine::Gemini, t).unwrap_or_default();
            assert!(is_gemini_own_model(&m), "{m} is not one of Google's own models");
        }
        // note `grok-4.7-high` is refused on CURSOR but is fine on the Grok seat itself, where it is a subscription
        assert!(model_blocked(Engine::Grok, Some("grok-4.7-high")).is_none(), "the guard is about Cursor's pool, not the model");
        // and Cursor stays usable: the guard must never make the whole engine unreachable
        assert!(model_blocked(Engine::Cursor, None).is_none(), "the engine itself is not blocked");

        // A READ-ONLY GEMINI RUN MUST STILL BE ABLE TO READ (2026-09-27): without the skip flag Antigravity soft-denies
        // the tool it needs and returns `status: SUCCESS` with an empty response, which is a silent no-answer.
        // THE PROMPT MUST REACH `agy`, and it only reaches it in argv (2026-09-27). With `-p ""` every Gemini task
        // failed at the invocation: `Error: empty prompt. Usage: agy --print "your prompt here"`.
        let (_, ap) = cli_command(Engine::Gemini, "", &RunOpts { prompt_argv: Some("do the thing".into()), ..Default::default() });
        assert_eq!(ap.last().map(String::as_str), Some("do the thing"), "the prompt has to arrive: {ap:?}");
        assert!(ap.windows(2).any(|w| w == ["-p", "do the thing"]), "{ap:?}");

        let (_, ar) = cli_command(Engine::Gemini, "", &RunOpts { prompt_argv: Some("x".into()), ..Default::default() });
        assert!(ar.windows(2).any(|w| w == ["--mode", "plan"]), "read-only stays read-only through plan mode: {ar:?}");
        assert!(ar.iter().any(|x| x == "--dangerously-skip-permissions"), "...but it must not be left unable to answer: {ar:?}");
        let (_, aww) = cli_command(Engine::Gemini, "", &RunOpts { write: true, ..Default::default() });
        assert!(aww.iter().any(|x| x == "--dangerously-skip-permissions") && !aww.iter().any(|x| x == "plan"), "{aww:?}");

        // and a denied tool is surfaced as an ERROR even though the envelope says SUCCESS
        let denied = r#"{"status":"SUCCESS","response":"","denied_actions":[{"action":"command","display_name":"RunCommand"}]}"#;
        let p = parse_cli_output(Engine::Gemini, denied, None);
        assert!(p.error.as_deref().is_some_and(|e| e.contains("RunCommand")), "a soft-deny must not read as an empty success: {p:?}");
        assert!(p.content.is_empty());

        let (bin, g) = cli_command(Engine::Grok, "be brief", &RunOpts { write: true, prompt_file: Some("/tmp/p.md".into()), ..Default::default() });
        assert_eq!(bin, "grok");
        assert!(g.windows(2).any(|w| w == ["--prompt-file", "/tmp/p.md"]), "{g:?}");
        assert!(g.windows(2).any(|w| w == ["--permission-mode", "bypassPermissions"]), "{g:?}");
        assert!(g.windows(2).any(|w| w == ["--system-prompt-override", "be brief"]), "{g:?}");
        let (_, gl) = cli_command(Engine::Grok, "", &RunOpts { tier: Some(Tier::Low), ..Default::default() });
        assert!(gl.windows(2).any(|w| w == ["--reasoning-effort", "low"]), "Grok's tiers are efforts, like Codex's: {gl:?}");
        assert!(!gl.iter().any(|x| x == "-m"), "a bare effort is not a model name: {gl:?}");

        // every engine key round-trips, so a pair or a settings row can name any of them
        for e in Engine::all() { assert_eq!(engine_of(&json!(e.key())), Some(e), "{}", e.key()); }
    }

    /// GEMINI FLASH IS PRICED LIKE THE CHEAPEST CHINESE MODEL (2026-09-27) — the finding that moved this product off
    /// them: they were never cheap against Gemini, only against Sonnet.
    #[test]
    fn gemini_flash_matches_the_cheapest_chinese_rung_and_sonnet_is_far_above_both() {
        let (gi, go) = prices_of(Engine::Gemini, Some("gemini-3.8-flash-low"));
        let (di, dobs) = prices_of(Engine::Opencode, Some("opencode/deepseek-v4-flash"));
        assert_eq!((gi, go), (di, dobs), "the same rung, to the cent");
        let (qi, qo) = prices_of(Engine::Opencode, Some("opencode/qwen3.6-plus"));
        assert!(qo > go, "qwen is dearer on output than Gemini Flash: {qo} vs {go}");
        let (_, so) = prices_of(Engine::ClaudeCode, Some("sonnet"));
        assert!(so > qo, "and Sonnet is dearer than both — the comparison that made them look cheap");
    }
}

#[cfg(test)]
mod heimdall_seat_tests {
    use super::*;
    /// "we should have a main provider for heimdall" — and a blocked one must never be it (2026-09-27: Zen off,
    /// main handed to OpenCode by the roles record, every routed ask refused).
    /// THE QA GATE IS ROUTED BY HEIMDALL, DETERMINISTICALLY, PER DEFECT CLASS (operator 2026-10-07). The expected orders are what the v2 lanes' table gives under the rule in `qa_gate_ranking`.
    #[test]
    fn the_qa_gate_ranking_follows_the_measured_table_class_by_class() {
        let names = |c: Option<&str>| qa_gate_ranking(c).into_iter().map(|(_, m)| m.split('@').next().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(names(Some("backend")), ["cursor-grok-4.6-high", "composer-2.5", "sonnet", "gpt-5.6-terra", "gemini-3.8-flash-medium"], "Opus is off the roster (never Opus); Codex and Gemini tell nothing there");
        assert_eq!(names(Some("frontend")), ["cursor-grok-4.6-high", "gpt-5.6-terra", "composer-2.5", "gemini-3.8-flash-medium", "sonnet"], "frontend: only Grok tells good from broken; the rest are last resorts, fewest false accepts first");
        assert_eq!(names(Some("design")), ["gemini-3.8-flash-medium", "cursor-grok-4.6-high", "composer-2.5", "sonnet", "gpt-5.6-terra"], "Gemini is perfect on design (0/0)");
        assert_eq!(names(Some("docs")), ["cursor-grok-4.6-high", "composer-2.5", "gemini-3.8-flash-medium", "gpt-5.6-terra", "sonnet"], "Opus, Sonnet and Codex reject every good docs run: not informative, and tied exactly, so the price orders them");
        assert_eq!(names(Some("infra")), ["gemini-3.8-flash-medium", "composer-2.5", "sonnet", "cursor-grok-4.6-high", "gpt-5.6-terra"], "everyone but Codex is perfect on infra: the price breaks the tie (cheapest first), and Codex's one false reject puts it last");
        assert_eq!(names(None), ["cursor-grok-4.6-high", "composer-2.5", "sonnet", "gemini-3.8-flash-medium", "gpt-5.6-terra"], "pooled: Grok first, Codex last (J 0.24: it rejects nearly everything)");
        assert_eq!(qa_gate_ranking(Some("not a class")), qa_gate_ranking(None), "an unknown class is the pooled ranking");
        assert_eq!(qa_gate_ranking(Some("backend")), qa_gate_ranking(Some("backend")), "deterministic");
        // the price never overrides a measured difference: on every class the seat the numbers put first is unchanged, except where the numbers tie exactly
        for c in ["backend", "frontend", "design", "docs"] { let first = qa_gate_ranking(Some(c))[0].1; assert!(["opus", "cursor-grok-4.6-high", "gemini-3.8-flash-medium"].contains(&first.split('@').next().unwrap()), "{c}: {first}"); }
        assert_eq!(qa_price_rank(Engine::Gemini, "gemini-3.8-flash-medium"), 0); assert!(qa_price_rank(Engine::Cursor, "cursor-grok-4.6-high") > qa_price_rank(Engine::ClaudeCode, "opus"));
        for (e, m, _) in QA_GATE_TABLE { assert!(QA_GATE_PRICE.contains(&(*e, *m)), "{e:?} {m} has no price rank"); }
        assert_eq!(qa_class("backend"), Some("backend")); assert_eq!(qa_class("tests"), None); assert_eq!(qa_class("mixed"), None);
        // every seat of the table is on the roster the design judge and critic walk, and the other way round
        for (e, m, _) in QA_GATE_TABLE { assert!(QA_GATE_ORDER.contains(&(*e, *m)), "{e:?} {m} is not on the roster"); }
        assert_eq!(QA_GATE_TABLE.len(), QA_GATE_ORDER.len());
    }
    #[test]
    fn the_first_usable_seat_of_the_ranking_is_the_gate_and_a_pin_still_wins() {
        let only = |seats: &'static [Engine]| move |e: Engine, _: Option<&str>| seats.contains(&e);
        // Claude on: backend goes to Opus; Claude off: to Grok - the same rule, never the run's own seat
        let b = qa_gate_pick(Some("backend"), only(&[Engine::ClaudeCode, Engine::Cursor, Engine::Gemini])).unwrap();
        assert_eq!((b.engine, b.model.as_deref(), b.source), (Engine::Cursor, Some("cursor-grok-4.6-high"), "qa-gate-ranking"));
        let b = qa_gate_pick(Some("backend"), only(&[Engine::Cursor, Engine::Gemini])).unwrap();
        assert_eq!((b.engine, b.model.as_deref()), (Engine::Cursor, Some("cursor-grok-4.6-high")));
        let g = qa_gate_pick(Some("design"), only(&[Engine::Gemini, Engine::Codex])).unwrap();
        assert_eq!((g.engine, g.model.as_deref()), (Engine::Gemini, Some("gemini-3.8-flash-medium")), "design: Gemini is the best gate there");
        let g = qa_gate_pick(Some("backend"), only(&[Engine::Gemini, Engine::Codex])).unwrap();
        assert_eq!(g.engine, Engine::Codex, "backend with only Gemini and Codex: Codex (0 false accepts) over Gemini (14 of 15)");
        assert!(qa_gate_pick(None, only(&[])).is_none(), "nothing usable: the caller's own rule answers");
        // a pin wins over the ranking, and only the QA gate has a ranking
        let tmp = std::env::temp_dir().join(format!("antiloki-qa-pin-{}", std::process::id())); let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(models_file().parent().unwrap()).unwrap();
        std::fs::write(models_file(), r#"{ "roles": { "qa": { "engine": "gemini", "model": "gemini-3.8-flash-medium" } } }"#).unwrap();
        assert!(role_pin(Role::Qa).is_some());
        let b = role_binding_in(Role::Qa, Engine::Codex, Some("backend"));
        assert_eq!(b.source, "role", "the operator's pin is not overridden by the ranking");
        std::fs::write(models_file(), r#"{ "roles": { "qa": { "engine": "auto" } } }"#).unwrap();
        assert!(role_pin(Role::Qa).is_none());
        assert_eq!(role_binding_in(Role::Planner, Engine::Codex, Some("backend")).source == "qa-gate-ranking", false, "only the gate has a ranking");
    }
    /// ONE MODEL SWITCHED OFF (operator 2026-10-07): refused by the one gate, so every router path skips it - and the others, and the seat itself, are untouched.
    #[test]
    fn a_model_the_operator_switched_off_is_never_a_gate_and_comes_back_when_switched_on() {
        assert!(!model_off(Engine::Cursor, "composer-2.5") && model_blocked(Engine::Cursor, Some("composer-2.5")).is_none());
        set_model_off(Engine::Cursor, "composer-2.5", true).unwrap();
        assert!(model_off(Engine::Cursor, "composer-2.5"));
        assert_eq!(model_blocked(Engine::Cursor, Some("composer-2.5")).as_deref(), Some("composer-2.5 is switched off (Settings › Seats)."));
        assert!(model_blocked(Engine::Cursor, Some("cursor-grok-4.6-high")).is_none(), "the other model of the same seat is untouched");
        assert!(model_blocked(Engine::Cursor, None).is_none(), "the seat itself is untouched");
        assert!(!seat_ok(Engine::Cursor, Some("composer-2.5")));
        // the QA ranking skips it: backend with Cursor and Gemini on goes to Grok, and with Grok off as well to Gemini's... nothing else of Cursor's
        let ok = |e: Engine, m: Option<&str>| model_blocked(e, m).is_none() && e == Engine::Cursor;
        assert_eq!(qa_gate_pick(Some("backend"), ok).unwrap().model.as_deref(), Some("cursor-grok-4.6-high"));
        set_model_off(Engine::Cursor, "cursor-grok-4.6-high", true).unwrap();
        assert!(qa_gate_pick(Some("backend"), ok).is_none(), "both Cursor gates off: no Cursor gate");
        // a key without an effort switches off every effort of the model
        set_model_off(Engine::Codex, "gpt-5.6-terra", true).unwrap();
        for e in ["gpt-5.6-terra", "gpt-5.6-terra@low", "gpt-5.6-terra@high"] { assert!(model_off(Engine::Codex, e), "{e}"); }
        assert!(known_models(Engine::Codex).contains(&"gpt-5.6-terra".to_string()), "an off model stays listed so it can be switched back on");
        set_model_off(Engine::Codex, "gpt-5.6-terra", false).unwrap(); set_model_off(Engine::Cursor, "composer-2.5", false).unwrap(); set_model_off(Engine::Cursor, "cursor-grok-4.6-high", false).unwrap();
        assert!(!model_off(Engine::Codex, "gpt-5.6-terra") && model_blocked(Engine::Cursor, Some("composer-2.5")).is_none());
        assert!(set_model_off(Engine::Codex, "  ", true).is_err());
        assert!(known_models(Engine::Cursor).contains(&"composer-2.5".to_string()) && known_models(Engine::ClaudeCode).iter().any(|m| m == "opus"));
    }

    #[test]
    fn heimdall_plans_on_its_chosen_seat_and_never_on_a_blocked_one() {
        let usable = |e: Engine| matches!(e, Engine::ClaudeCode | Engine::Codex);
        assert_eq!(heimdall_engine_for("codex", Some(Engine::Opencode), usable), Some(Engine::Codex), "the chosen seat wins over the pane's");
        assert_eq!(heimdall_engine_for("pane", Some(Engine::Opencode), usable), Some(Engine::ClaudeCode), "the pane's seat is blocked: the first usable one plans instead of failing");
        assert_eq!(heimdall_engine_for("pane", Some(Engine::Codex), usable), Some(Engine::Codex), "the pane's own seat when it can");
        assert_eq!(heimdall_engine_for("opencode", Some(Engine::Codex), usable), Some(Engine::Codex), "a chosen seat that is blocked falls back to the pane's");
        assert_eq!(heimdall_engine_for("pane", None, |_| false), None, "nothing usable: say so, do not guess");
        assert_eq!(heimdall_engine_for("nonsense", Some(Engine::Codex), usable), Some(Engine::Codex), "an unknown choice is no choice");
    }
    /// "Backender on opencode — 100% over 3 runs" was measured with Zen on; with Zen off that pair must bend, not fail.
    #[test]
    fn a_routed_pair_on_a_switched_off_seat_bends_to_the_reachable_one() {
        let blocked = |e: Engine, _m: Option<&str>| e == Engine::Opencode;
        let p = Pair { name: "backender".into(), brain: Engine::Opencode, hands: Engine::Opencode, brain_model: Some("deepseek-v4-pro".into()), hands_model: None };
        let (q, why) = pair_reachable(p, Engine::ClaudeCode, blocked);
        assert_eq!((q.brain, q.hands, q.brain_model), (Engine::ClaudeCode, Engine::ClaudeCode, None), "brain to the fallback, hands to the brain");
        assert!(why.unwrap().contains("switched off"));
        let fine = Pair { name: "x".into(), brain: Engine::ClaudeCode, hands: Engine::Codex, brain_model: None, hands_model: None };
        let (q, why) = pair_reachable(fine.clone(), Engine::ClaudeCode, blocked);
        assert_eq!((q.brain, q.hands), (Engine::ClaudeCode, Engine::Codex)); assert!(why.is_none(), "a reachable pair is left exactly as chosen");
        let hands_only = Pair { name: "y".into(), brain: Engine::Codex, hands: Engine::Opencode, brain_model: None, hands_model: Some("kimi".into()) };
        let (q, _) = pair_reachable(hands_only, Engine::ClaudeCode, blocked);
        assert_eq!((q.brain, q.hands, q.hands_model), (Engine::Codex, Engine::Codex, None), "only the blocked side moves");
    }
}

#[cfg(test)]
mod model_seat_tests {
    use super::*;
    #[test]
    fn a_model_is_recognised_as_its_own_seats_or_not() {
        assert!(model_is_of(Engine::ClaudeCode, "haiku") && model_is_of(Engine::ClaudeCode, "claude-opus-5[1m]"));
        // the audit's misses: a family this file itself names, and OpenAI's later reasoning models
        assert!(model_is_of(Engine::ClaudeCode, "claude-fable-5-1") && model_is_of(Engine::ClaudeCode, "fable"));
        assert!(model_is_of(Engine::Codex, "o4-mini") && model_is_of(Engine::Codex, "o5"));
        assert!(!model_is_of(Engine::ClaudeCode, "composer-2.5"));
        assert!(model_is_of(Engine::Cursor, "composer-2.5") && model_is_of(Engine::Cursor, "cursor-grok-4.6-high"));
        assert!(!model_is_of(Engine::Cursor, "haiku"), "the whole point of the Cursor guard");
        assert!(model_is_of(Engine::Gemini, "gemini-3.8-flash-low") && !model_is_of(Engine::Gemini, "sonnet"));
        assert!(model_is_of(Engine::Codex, "medium"), "Codex's tiers ARE its model ids — an effort, not a model");
        assert!(model_is_of(Engine::Grok, "low") && model_is_of(Engine::Grok, "grok-code-fast-1"));
        assert!(model_is_of(Engine::Opencode, "anything/at-all"), "the gateway serves every vendor");
        assert!(model_is_of(Engine::ClaudeCode, ""), "nothing named is never wrong");
    }
}

#[cfg(test)]
mod pair_affordable_tests {
    use super::*;
    fn cheap(e: Engine) -> Option<String> { tier_model(e, Tier::Low) }
    fn never_blocked(_: Engine, _: Option<&str>) -> bool { false }
    #[test]
    fn a_pair_of_two_frontier_sides_bends_instead_of_killing_the_ask() {
        // the exact pair that refused every ask on a fresh fork, 2026-09-28
        let p = Pair { brain: Engine::Codex, brain_model: Some("gpt-5.6-luna@medium".into()),
                       hands: Engine::Codex, hands_model: None, name: String::new() };
        assert!(p.problem().is_some(), "the rule itself is right");
        assert!(Pair { brain: Engine::Codex, brain_model: Some("gpt-5.6-luna@medium".into()),
                       hands: Engine::Codex, hands_model: Some("gpt-5.6-luna@medium".into()), name: String::new() }.problem().is_none(),
                "one model on both sides buys one price — a solo run, not a merge");
        let (q, why) = pair_affordable(p, cheap, never_blocked);
        assert!(q.problem().is_none(), "whatever it bends to must satisfy the rule it bent for");
        assert!(why.unwrap().contains("two frontier prices"));
        assert_eq!(q.brain, Engine::Codex, "the brain is never touched — the plan is judged on it");
    }
    /// audit 2026-09-28: the bend used to install a model the provider gate then refused, so the ask died one
    /// step later under a log line saying it had been made affordable
    #[test]
    fn it_never_bends_onto_a_seat_or_model_the_gate_will_refuse() {
        let p = Pair { brain: Engine::ClaudeCode, brain_model: Some("opus".into()),
                       hands: Engine::Codex, hands_model: Some("gpt-5.6-luna@medium".into()), name: String::new() };
        // every cheap rung is blocked except Cursor's own pool; the brain itself stays reachable, which is what
        // `pair_reachable` guarantees before this ever runs
        let (q, why) = pair_affordable(p, cheap,
            |e, m| e != Engine::Cursor && !(e == Engine::ClaudeCode && m == Some("opus")));
        assert_eq!(q.hands, Engine::Cursor, "it walked to the one seat that can actually take the call");
        assert!(q.problem().is_none());
        assert!(why.unwrap().contains("hands move to cursor"));
    }
    #[test]
    fn a_pair_that_already_obeys_the_rule_is_left_exactly_as_it_is() {
        let p = Pair { brain: Engine::ClaudeCode, brain_model: Some("opus".into()),
                       hands: Engine::Cursor, hands_model: Some("composer-2.5".into()), name: String::new() };
        let (q, why) = pair_affordable(p.clone(), cheap, never_blocked);
        assert!(why.is_none()); assert_eq!(q.hands, p.hands); assert_eq!(q.hands_model, p.hands_model);
    }
    #[test]
    fn with_nothing_cheap_to_reach_the_brain_does_the_work_itself() {
        // two frontier seats, and the record offers no cheap rung on either
        let p = Pair { brain: Engine::ClaudeCode, brain_model: Some("opus".into()),
                       hands: Engine::Codex, hands_model: Some("gpt-5.6-luna@medium".into()), name: String::new() };
        let (q, why) = pair_affordable(p, |_| None, never_blocked);
        assert_eq!(q.hands, Engine::ClaudeCode, "the hands become the brain — one seat, one price");
        assert_eq!(q.hands_model, q.brain_model);
        assert!(q.problem().is_none(), "the bend must satisfy the rule it bent for");
        assert!(why.unwrap().contains("not a saving"), "the last resort is named as one, never logged as a bargain");
    }
}

#[cfg(test)]
mod bend_pair_tests {
    use super::*;
    fn cheap(e: Engine) -> Option<String> { tier_model(e, Tier::Low) }
    /// goals log, operator 2026-10-01: "the pair was bent: cursor is switched off, the brain does the work itself" was the ONLY line, and the frontier rule then moved
    /// the hands again - the log understated what the run was doing. Both bends, in order, each with its reason.
    #[test]
    fn two_bends_are_both_reported_in_order_with_their_reasons() {
        let p = Pair { name: String::new(), brain: Engine::Codex, brain_model: Some("gpt-5.6-luna@medium".into()), hands: Engine::Cursor, hands_model: Some("composer-2.5".into()) };
        // cursor is off; every other rung of the brain's own seat is not offered today, so the frontier rule has to look further
        let (q, lines, reachable) = bend_pair(p, Engine::Codex,
            |e, _| if e == Engine::Cursor { Some("switched off") } else { None }, cheap,
            |e, m| e == Engine::Codex && m != Some("gpt-5.6-luna@medium") || e == Engine::Cursor);
        assert_eq!(reachable, 1);
        assert_eq!(lines.len(), 2, "one line per bend: {lines:?}");
        assert!(lines[0].contains("cursor is switched off") && lines[0].contains("the brain does the work itself"), "{lines:?}");
        assert!(lines[1].contains("two frontier prices") && lines[1].contains(&format!("hands move to {}", q.hands.key())), "the SECOND bend names where the hands went: {lines:?}");
        assert_ne!(q.hands, Engine::Cursor); assert!(q.problem().is_none());
        assert!(work_line(&q).contains(q.hands.key()), "the closing line names who works");
    }
    #[test]
    fn a_seat_out_of_quota_is_named_as_that_and_not_as_switched_off() {
        let p = Pair { name: String::new(), brain: Engine::ClaudeCode, brain_model: Some("opus".into()), hands: Engine::Gemini, hands_model: Some("gemini-3.8-flash-low".into()) };
        let (q, lines, _) = bend_pair(p, Engine::ClaudeCode, |e, _| if e == Engine::Gemini { Some("out of quota") } else { None }, cheap, |_, _| false);
        assert!(lines[0].contains("gemini is out of quota"), "{lines:?}");
        assert_eq!(q.hands, Engine::ClaudeCode);
    }
    #[test]
    fn a_pair_nothing_bends_has_no_lines() {
        let p = Pair { name: String::new(), brain: Engine::ClaudeCode, brain_model: Some("opus".into()), hands: Engine::Cursor, hands_model: Some("composer-2.5".into()) };
        let (_, lines, reachable) = bend_pair(p, Engine::ClaudeCode, |_, _| None, cheap, |_, _| false);
        assert!(lines.is_empty() && reachable == 0);
    }
}

#[cfg(test)]
mod priced_from_tokens_tests {
    use super::*;
    #[test]
    fn a_missing_credential_file_is_no_on_linux_and_unknown_on_windows() {
        assert_eq!(seat_unsure(true, false), Some(true));
        assert_eq!(seat_unsure(true, true), Some(true), "a file that is there is a yes everywhere");
        assert_eq!(seat_unsure(false, false), Some(false), "on Linux the path is known: no file, no login");
        assert_eq!(seat_unsure(false, true), None, "on Windows the login may live elsewhere: unknown, so the seat is not hidden");
        // `engine_usable` takes an unknown as usable: only a definite `Some(false)` rules a seat out
        assert!(None::<bool> != Some(false));
    }
    #[test]
    fn a_seat_that_bills_nothing_is_still_priced_at_the_published_rate() {
        // the real numbers off the pipeline's flask-5014 task, whose cost came back null
        let c = priced_from_tokens(Engine::Cursor, Some("composer-2.5"), Some(11578), Some(1830)).unwrap();
        let (pin, pout) = prices_of(Engine::Cursor, Some("composer-2.5"));
        assert!((c - (11578.0 * pin + 1830.0 * pout) / 1e6).abs() < 1e-12);
        assert!(c > 0.0, "a turn with tokens is never free to the comparison, however the seat is billed");
    }
    #[test]
    fn nothing_to_price_stays_none_rather_than_becoming_zero() {
        assert_eq!(priced_from_tokens(Engine::Cursor, Some("composer-2.5"), None, None), None);
        assert_eq!(priced_from_tokens(Engine::Cursor, Some("composer-2.5"), Some(0), Some(0)), None,
                   "zero tokens is an absent measurement, not a free turn");
    }
}

#[cfg(test)]
mod gemini_price_tests {
    use super::*;
    #[test]
    fn a_gemini_model_is_not_priced_as_a_mini_because_the_word_contains_one() {
        // the audit's find: "gemini" contains "mini", so the cheap-family test swallowed the whole family
        assert_eq!(prices_of(Engine::Gemini, Some("gemini-3.8-flash-low")), (0.14, 0.28), "flash is flash");
        assert_eq!(prices_of(Engine::Gemini, Some("gemini-3.1-pro-high")), (1.25, 10.0), "pro is pro");
        assert_eq!(prices_of(Engine::Gemini, Some("gemini-3.8")), (1.25, 10.0),
                   "the account default is NOT the cheapest rate in the table");
        assert_eq!(prices_of(Engine::Gemini, Some("gemini-exp")), (1.25, 10.0));
        // and the genuinely cheap families of other vendors still price cheaply
        assert_eq!(prices_of(Engine::Codex, Some("gpt-5-mini")), (0.14, 0.28));
    }
}

#[cfg(test)]
mod origin_name_tests {
    use super::*;
    /// operator 2026-09-28: the origin is what spends the money, so it is always on the label
    #[test]
    fn the_same_model_from_two_seats_reads_as_two_different_things() {
        assert_eq!(side_name(Engine::ClaudeCode, Some("sonnet")), "Claude · Sonnet");
        assert_eq!(side_name(Engine::Cursor, Some("composer-2.5")), "Cursor · Composer 2.5");
        assert_eq!(side_name(Engine::Opencode, Some("opencode/deepseek-v4-flash")), "OpenCode · DeepSeek v4 Flash");
        assert_eq!(side_name(Engine::Gemini, Some("gemini-3.8-flash-low")), "Antigravity · 3.8 Flash Low");
        // the two Groks: one is Cursor's own pool, the other is a second subscription
        assert_eq!(side_name(Engine::Cursor, Some("cursor-grok-4.6-medium")), "Cursor · Grok 4.6 · medium");
        assert_eq!(side_name(Engine::Grok, Some("grok-code-fast-1")), "Grok · Code Fast 1");
        assert_ne!(side_name(Engine::Cursor, Some("cursor-grok-4.6-medium")), side_name(Engine::Grok, Some("grok-4.6-medium")));
        // and a side whose model the job decides still says whose seat it is
        assert_eq!(side_name(Engine::Cursor, None), "Cursor · the job's model");
    }
}

#[cfg(test)]
mod engine_config_tests {
    use super::*;
    /// THE OPERATOR'S FILE AS IT STOOD ON 2026-10-01 (a copy, never the file itself): the planner, the QA and Heimdall's seat named Claude, whose subscription had ended,
    /// and the worker named OpenCode with Zen off. It only worked because `role_binding` swapped the seat at call time.
    const OPERATOR_FILE: &str = r#"{
  "opencode": { "mid": "opencode/deepseek-v4-pro" },
  "roles": {
    "worker":  { "engine": "opencode",    "model": "opencode/deepseek-v4-flash" },
    "planner": { "engine": "claude-code", "model": "sonnet" },
    "qa":      { "engine": "claude-code", "model": "opus" }
  },
  "providers": { "heimdall": "claude-code" }
}"#;
    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("antiloki-engine-config-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d
    }
    fn claude_and_zen_gone(e: Engine) -> bool { !matches!(e, Engine::ClaudeCode | Engine::Opencode) }

    #[test]
    fn an_empty_config_never_resolves_to_a_hard_coded_claude() {
        // the fallback walk used to start with Claude: a machine with every seat usable and nothing configured planned on Claude
        assert_eq!(heimdall_engine_for("pane", None, |_| true), Some(Engine::all()[0]), "the router's own order");
        assert_ne!(Engine::all()[0], Engine::ClaudeCode);
        // Claude is a seat like the others: second in the router's order, it plans only when the seats before it are gone
        assert_eq!(heimdall_engine_for("pane", None, |e| e != Engine::all()[0]), Some(Engine::all()[1]));
        assert_eq!(heimdall_engine_for("pane", None, |e| e != Engine::ClaudeCode), Some(Engine::all()[0]));
        assert_eq!(heimdall_engine_for("pane", None, |e| e == Engine::Cursor), Some(Engine::Cursor), "the only seat left plans");
    }

    #[test]
    fn a_call_that_names_no_seat_asks_for_auto_and_gets_the_serves_pick() {
        assert_eq!(engine_or_auto(&json!("cli:auto")), Some(auto_engine()));
        assert_eq!(engine_or_auto(&json!("auto")), Some(auto_engine()));
        assert_eq!(engine_or_auto(&json!("cli:codex")), Some(Engine::Codex), "a named seat is still itself");
        assert_eq!(engine_or_auto(&json!("cli:nonsense")), None);
        assert_eq!(engine_or_auto(&Value::Null), None, "no model at all is still a refusal");
    }

    #[test]
    fn the_migration_turns_a_pin_on_a_gone_seat_into_auto_and_keeps_a_backup() {
        let d = scratch("migrate");
        let f = d.join("engine-models.json");
        std::fs::write(&f, OPERATOR_FILE).unwrap();
        let changed = migrate_models_file(&f, claude_and_zen_gone, "2026-10-01").unwrap();
        assert_eq!(changed.len(), 4, "three roles and Heimdall's seat: {changed:?}");
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        for r in ["planner", "qa", "worker"] { assert_eq!(after["roles"][r], json!({ "engine": "auto" }), "{r} is auto"); }
        assert_eq!(after["providers"]["heimdall"], json!("pane"), "Heimdall's seat goes back to following the pane");
        assert_eq!(after["opencode"]["mid"], json!("opencode/deepseek-v4-pro"), "the rest of the file is untouched");
        // the old file is kept, byte for byte, beside it
        let bak = d.join("engine-models.json.bak-2026-10-01");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), OPERATOR_FILE);
        // IDEMPOTENT: a second run finds nothing to do and rewrites nothing (the file is the same bytes, the backup is not replaced)
        let written = std::fs::read_to_string(&f).unwrap();
        std::fs::write(&bak, "first backup").unwrap();
        assert!(migrate_models_file(&f, claude_and_zen_gone, "2026-10-02").unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), written);
        assert!(!d.join("engine-models.json.bak-2026-10-02").exists(), "no backup for a run that changed nothing");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "first backup", "and the one-time backup is never overwritten");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// REVIEW 2026-10-01: the start-up probe was `engine_usable` - a binary on the launch PATH and a credential under the launch $HOME - so a serve started with a thinner HOME than the shell
    /// (the desktop sidecar) saw every seat signed out and reset every pin to auto for good. It must follow the operator's switches only.
    #[test]
    fn the_startup_migration_does_not_forget_pins_because_this_process_cannot_see_the_seats() {
        let _env = super::tests::env_lock();
        let d = scratch("startup");
        let f = d.join("engine-models.json");
        // Cursor is switched off by the operator; Codex, Gemini and Grok are pinned and on - but this process has an EMPTY home (no credential of any seat)
        std::fs::write(&f, r#"{ "roles": { "planner": { "engine": "codex", "model": "high" }, "qa": { "engine": "cursor", "model": "composer-2.5" }, "worker": { "engine": "grok" } },
                                "providers": { "heimdall": "gemini", "seats_off": ["cursor"], "zen": true } }"#).unwrap();
        // the switches are read from `models_file()` (this thread's own copy under test), so the same document goes there
        let mine = models_file(); if let Some(p) = mine.parent() { let _ = std::fs::create_dir_all(p); }
        std::fs::write(&mine, std::fs::read_to_string(&f).unwrap()).unwrap();
        let home = d.join("empty-home"); std::fs::create_dir_all(&home).unwrap();
        let old_home = std::env::var_os("HOME");
        REAL_SEAT_CHECK.with(|c| c.set(true));
        std::env::set_var("HOME", &home);
        let trusted: Vec<(Engine, bool)> = Engine::all().iter().map(|e| (*e, migration_trusts(*e))).collect();
        let seat_signed_out = engine_seat(Engine::Codex);
        let changed = migrate_models_file(&f, migration_trusts, "2026-10-01").unwrap();
        match old_home { Some(h) => std::env::set_var("HOME", h), None => std::env::remove_var("HOME") }
        let _ = std::fs::remove_file(&mine);
        assert_eq!(seat_signed_out, Some(false), "the fixture: with that home Codex reads as signed out");
        assert!(trusted.iter().any(|(e, ok)| *e == Engine::Codex && *ok), "{trusted:?}");
        assert_eq!(changed.len(), 1, "only the seat the operator switched off moves: {changed:?}");
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(after["roles"]["planner"], json!({ "engine": "codex", "model": "high" }), "a signed-out reading is not a reason to forget a pin");
        assert_eq!(after["roles"]["worker"], json!({ "engine": "grok" }));
        assert_eq!(after["providers"]["heimdall"], json!("gemini"));
        assert_eq!(after["roles"]["qa"], json!({ "engine": "auto" }), "a seat the operator switched off is migrated");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// F-18 (audit 2026-10-01): a second migration on the same day found `.bak-<date>` and skipped the copy, so its own pre-state was lost; and the write was a truncate-then-write.
    #[test]
    fn a_second_migration_on_the_same_day_keeps_its_own_pre_state_and_the_write_is_atomic() {
        let d = scratch("migrate-twice");
        let f = d.join("engine-models.json");
        std::fs::write(&f, OPERATOR_FILE).unwrap();
        assert_eq!(migrate_models_file(&f, claude_and_zen_gone, "2026-10-01").unwrap().len(), 4);
        // the operator pins Claude again (a new pre-state) and the same day's next start-up migrates it away once more
        let second_pre = r#"{ "roles": { "planner": { "engine": "claude-code", "model": "opus" } } }"#;
        std::fs::write(&f, second_pre).unwrap();
        assert_eq!(migrate_models_file(&f, claude_and_zen_gone, "2026-10-01").unwrap().len(), 1);
        assert_eq!(std::fs::read_to_string(d.join("engine-models.json.bak-2026-10-01")).unwrap(), OPERATOR_FILE, "the first backup is untouched");
        assert_eq!(std::fs::read_to_string(d.join("engine-models.json.bak-2026-10-01-2")).unwrap(), second_pre, "the second run's pre-state is kept as -2");
        std::fs::write(&f, second_pre).unwrap();
        migrate_models_file(&f, claude_and_zen_gone, "2026-10-01").unwrap();
        assert_eq!(std::fs::read_to_string(d.join("engine-models.json.bak-2026-10-01-3")).unwrap(), second_pre, "and a third as -3");
        // the new file is a whole document and no temp file is left beside it
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).expect("a complete JSON document");
        assert_eq!(after["roles"]["planner"], json!({ "engine": "auto" }));
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|n| !n.contains(".tmp-")), "{names:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_migration_leaves_a_file_that_needs_no_change_alone() {
        let d = scratch("untouched");
        let f = d.join("engine-models.json");
        // every seat is usable: nothing to rewrite - not the file, not a backup
        std::fs::write(&f, OPERATOR_FILE).unwrap();
        assert!(migrate_models_file(&f, |_| true, "2026-10-01").unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), OPERATOR_FILE);
        assert!(!d.join("engine-models.json.bak-2026-10-01").exists());
        // a pin on a seat that is usable stays pinned while the one on a gone seat moves
        let c = migrate_models_file(&f, |e| e != Engine::ClaudeCode, "2026-10-01").unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(c.len(), 3, "{c:?}");
        assert_eq!(after["roles"]["worker"]["engine"], json!("opencode"), "a usable seat keeps its pin");
        assert_eq!(after["roles"]["planner"]["engine"], json!("auto"));
        // a missing file, a file that is not JSON and one that is not an object are never touched or created
        assert!(migrate_models_file(&d.join("none.json"), |_| false, "2026-10-01").unwrap().is_empty());
        assert!(!d.join("none.json").exists());
        std::fs::write(&f, "not json {").unwrap();
        assert!(migrate_models_file(&f, |_| false, "2026-10-01").unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "not json {");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_role_is_auto_or_pinned_and_auto_is_the_default() {
        let f = models_file(); let _ = std::fs::remove_file(&f);
        for r in [Role::Planner, Role::Worker, Role::Qa] { assert_eq!(role_pin(r), None, "an unset role is auto"); }
        set_role_binding("planner", Some("codex"), Some("high")).unwrap();
        assert_eq!(role_pin(Role::Planner), Some((Some(Engine::Codex), Some("high".into()))));
        // saying `auto` hands the role back to the router, and leaves no pin behind
        let all = set_role_binding("planner", Some("auto"), Some("high")).unwrap();
        assert_eq!(role_pin(Role::Planner), None);
        assert!(all["roles"].get("planner").is_none(), "{all}");
        // …and a file that says it in words (what the migration writes) reads the same, whatever model rides with it
        std::fs::write(&f, r#"{"roles":{"qa":{"engine":"auto","model":"opus"},"worker":{"engine":"AUTO"}}}"#).unwrap();
        assert_eq!(role_pin(Role::Qa), None);
        assert_eq!(role_pin(Role::Worker), None);
        assert_ne!(role_binding_raw(Role::Qa, Engine::Cursor).source, "role", "auto is decided by the job, not by a pin");
        assert_eq!(role_binding_raw(Role::Qa, Engine::Cursor).engine, Engine::Cursor);
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn the_settings_payload_knows_every_seat_and_says_auto_or_pinned() {
        let f = models_file(); let _ = std::fs::remove_file(&f);
        set_role_binding("qa", Some("cursor"), Some("composer-2.5")).unwrap();
        let p = crate::custom::engine_models_payload(vec![]);
        let keys: Vec<&str> = Engine::all().iter().map(|e| e.key()).collect();
        assert_eq!(keys.len(), 6);
        for k in &keys {
            assert_eq!(p["rows"].as_array().unwrap().iter().filter(|r| r["engine"] == *k).count(), 3, "{k}: a row per tier");
            assert!(p["jobs"].as_array().unwrap().iter().all(|j| j["cells"].get(*k).is_some()), "{k}: a cell in every job");
            assert!(p["seats"].as_array().unwrap().iter().any(|s| s["key"] == *k && s["name"].as_str().is_some_and(|n| !n.is_empty())), "{k}: a seat entry");
        }
        let role = |n: &str| p["roles"].as_array().unwrap().iter().find(|r| r["role"] == n).cloned().unwrap();
        assert_eq!(role("planner")["mode"], json!("auto"), "the default");
        assert_eq!(role("planner")["pinned"], Value::Null);
        assert_eq!(role("qa")["mode"], json!("pinned"));
        assert_eq!(role("qa")["pinned"], json!({ "engine": "cursor", "model": "composer-2.5" }), "what the operator chose, whatever it resolves to today");
        assert_eq!(p["auto_engine"], json!(auto_engine().key()));
        let _ = std::fs::remove_file(&f);
    }
}

#[cfg(test)]
mod killer_slot_tests {
    use super::*;
    fn sleeper(secs: &str) -> tokio::process::Child { tokio::process::Command::new("sleep").arg(secs).spawn().expect("sleep exists") }

    /// STOP HAS TO REACH THE CLI (audit 2026-10-05): the child lives in the killer slot, a cancel takes it out and
    /// kills it, and the waiter sees `None` — not a hang until the 300 s timeout.
    #[tokio::test]
    async fn a_cancel_that_takes_the_child_ends_the_wait_with_none() {
        let slot: Killer = Arc::new(Mutex::new(Some(sleeper("30"))));
        let w = { let s = slot.clone(); tokio::spawn(async move { wait_in_slot(&s).await }) };
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        assert!(slot.lock().unwrap().is_some(), "the run is alive while it runs (goals.rs reads this)");
        let taken = slot.lock().unwrap().take();
        if let Some(mut c) = taken { let _ = c.start_kill(); }
        let r = tokio::time::timeout(std::time::Duration::from_secs(5), w).await.expect("the waiter ends").unwrap();
        assert!(r.is_none(), "a killed run reports as cancelled");
    }

    #[tokio::test]
    async fn a_child_that_exits_on_its_own_reports_its_status_and_empties_the_slot() {
        let slot: Killer = Arc::new(Mutex::new(Some(sleeper("0"))));
        let r = tokio::time::timeout(std::time::Duration::from_secs(5), wait_in_slot(&slot)).await.expect("ends").expect("not cancelled");
        assert!(r.unwrap().success());
        assert!(slot.lock().unwrap().is_none(), "no longer alive once it has finished");
    }

    /// `agy` takes its prompt in argv; one over the OS limit fails before any process exists, with a reason
    #[tokio::test]
    async fn an_argv_prompt_over_the_os_limit_is_refused_with_a_reason_before_spawning() {
        let big = ChatMessage { role: "user".into(), content: "x".repeat(ARGV_PROMPT_MAX + 10) };
        let e = run_cli(Engine::Gemini, &[big], ".", false, RunOpts::default(), None).await.err().expect("refused");
        assert!(e.contains("argv") && e.contains("KB"), "{e}");
    }
}

#[cfg(test)]
mod cursor_stream_tests {
    use super::*;
    /// A REAL CURSOR STREAM (probed 2026-10-06, composer-2.5, ask mode): thinking deltas, a read, the answer, the result with camelCase usage.
    const STREAM: &str = r#"{"type":"system","subtype":"init","cwd":"/x","model":"Composer 2.5"}
{"type":"thinking","subtype":"delta","text":"Reading seed.txt to "}
{"type":"tool_call","subtype":"started","call_id":"t1","tool_call":{"readToolCall":{"args":{"path":"/x/seed.txt"}}}}
{"type":"tool_call","subtype":"completed","call_id":"t1","tool_call":{"readToolCall":{"args":{"path":"/x/seed.txt"},"result":{"success":{"content":"pineapple"}}}}}
{"type":"tool_call","subtype":"started","call_id":"t2","tool_call":{"shellToolCall":{"args":{"command":"ls -la"}}}}
{"type":"thinking","subtype":"delta","text":"The first word is pineapple."}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"The first word is **pineapple**."}]}}
{"type":"result","subtype":"success","is_error":false,"result":"The first word is **pineapple**.","usage":{"inputTokens":8985,"outputTokens":184,"cacheReadTokens":16572}}"#;
    #[test]
    fn a_cursor_stream_gives_the_answer_the_tokens_and_every_tool_it_started() {
        let p = parse_cli_output(Engine::Cursor, STREAM, None);
        assert_eq!(p.content, "The first word is **pineapple**.", "the result line, never a thinking delta");
        assert_eq!((p.tokens_in, p.tokens_out), (Some(8985), Some(184)));
        let t: Vec<(&str, &str)> = p.tools.iter().map(|t| (t.name.as_str(), t.target.as_str())).collect();
        assert_eq!(t, vec![("Read", "/x/seed.txt"), ("Bash", "ls -la")], "started events only: a completed event is the same call");
        assert!(p.error.is_none());
    }
    #[test]
    fn a_stream_cut_short_still_answers_from_the_assistant_line_and_a_failed_result_is_the_error() {
        let cut = STREAM.lines().filter(|l| !l.contains("\"result\"")).collect::<Vec<_>>().join("\n");
        let p = parse_cli_output(Engine::Cursor, &cut, None);
        assert!(p.tools.len() == 2 && !p.content.contains("Reading seed.txt"), "{p:?}");
        let failed = r#"{"type":"result","subtype":"error","is_error":true,"result":"Rate limit reached"}"#;
        assert_eq!(parse_cli_output(Engine::Cursor, failed, None).error.as_deref(), Some("Rate limit reached"));
    }
    #[test]
    fn cursor_is_asked_for_a_stream() {
        let (bin, a) = cli_command(Engine::Cursor, "", &RunOpts::default());
        assert_eq!(bin, "cursor-agent");
        assert!(a.windows(2).any(|w| w == ["--output-format", "stream-json"]), "{a:?}");
    }
}

#[cfg(test)]
mod agy_stream_tests {
    use super::*;
    /// A REAL ANTIGRAVITY STREAM (probed 2026-10-06, plan mode): a tool step that was auto-denied, and a result that says so.
    const STREAM: &str = r#"{"event":"init","conversation_id":"c1","init":{"cwd":"/x","tools":["run_command"]}}
{"event":"step_update","step_update":{"step_index":0,"state":"DONE","step_type":"user_input"}}
{"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":12680,"output_tokens":2606}}}
{"event":"step_update","step_update":{"step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"view_file","tool_info":{"name":"view_file","parameters":{"AbsolutePath":"/x/seed.txt"}}}}
{"event":"step_update","step_update":{"step_index":2,"state":"DONE","step_type":"tool","tool_name":"view_file","tool_info":{"name":"view_file","parameters":{"AbsolutePath":"/x/seed.txt"}}}}
{"event":"step_update","step_update":{"step_index":3,"state":"ACTIVE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"ls -la"}}}}
{"event":"result","result":{"status":"SUCCESS","response":"The first word is pineapple.","usage":{"input_tokens":12680,"output_tokens":2606}}}"#;
    #[test]
    fn an_agy_stream_gives_the_answer_the_tokens_and_each_tool_once() {
        let p = parse_cli_output(Engine::Gemini, STREAM, None);
        assert_eq!(p.content, "The first word is pineapple.");
        assert_eq!((p.tokens_in, p.tokens_out), (Some(12680), Some(2606)));
        let t: Vec<(&str, &str)> = p.tools.iter().map(|t| (t.name.as_str(), t.target.as_str())).collect();
        assert_eq!(t, vec![("view_file", "/x/seed.txt"), ("run_command", "ls -la")], "an ACTIVE step is a call; its DONE twin is not another");
    }
    #[test]
    fn a_denied_action_is_still_the_error_and_the_legacy_single_object_still_parses() {
        let denied = r#"{"event":"result","result":{"status":"SUCCESS","response":"","denied_actions":[{"action":"command","display_name":"RunCommand"}]}}"#;
        assert!(parse_cli_output(Engine::Gemini, denied, None).error.as_deref().is_some_and(|e| e.contains("RunCommand")));
        let legacy = r#"{"response":"hello","stats":{"models":{}}}"#;
        assert_eq!(parse_cli_output(Engine::Gemini, legacy, None).content, "hello");
        assert!(parse_cli_output(Engine::Gemini, "{\"event\":\"init\"}", None).error.is_some(), "a stream that never reached a result says so");
    }
}
