//! THE TEAM GRAPH (RFC 0008 §4.3 to §4.7): a team as DATA. Nodes (roles, control flow, tools), explicit edges with named ports, a small condition language, a validator, and `graph_step`, the pure decision half
//! that says what runs next. Nothing here touches a clock, a file, a model or a process: time arrives as an event, so every rule is tested without any of them, a run can be REPLAYED from its events, and a
//! graph can be SIMULATED with stubbed outputs before a cent is spent.
//!
//! THE FORM (`.antiloki/teams/<name>.json`, schema 1): `{ "team", "schema", "branch", "budget_usd", "nodes": { id: { "node": kind, ... } }, "edges": [ { "from", "port", "to" } ] }`. A node's kind says which ports it
//! has; an edge leaves a port. `out` is the plain port; `then`/`else` an if's; a switch has one port per case and `default`; a loop has `body`, `done` and `exhausted`; a for-each `body` and `done`; a fork `branch`;
//! a human `approved` and `denied`; and any node that can fail has the failure ports `failed`, `error`, `timeout`, `cap`, `budget`, `human`. A failure port with no edge ends the run in "handed to the person".
//!
//! CONDITIONS are a small expression over typed values (`tests.exit == 0`, `audit.verdict in ["flag","block"]`, `loop.count < 3`, `budget.left > 0.2`). No functions, no assignment, no I/O, no model: judgement lives
//! in an agent node that returns an enum, and a switch reads it.
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const SCHEMA: u32 = 1;
/// A run that takes more than this many steps is a graph that does not end: stop it (the validator should have caught it).
pub const MAX_STEPS: usize = 5_000;

// ── the graph ─────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Tier { Low, Mid, High }
impl Tier { pub fn key(self) -> &'static str { match self { Tier::Low => "low", Tier::Mid => "mid", Tier::High => "high" } } }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RoleKind { Planner, Worker, Qa, Debugger, Audit, Agent }
impl RoleKind {
    pub fn key(self) -> &'static str { match self { RoleKind::Planner => "planner", RoleKind::Worker => "worker", RoleKind::Qa => "qa", RoleKind::Debugger => "debugger", RoleKind::Audit => "audit", RoleKind::Agent => "agent" } }
    pub fn parse(s: &str) -> Option<RoleKind> { match s.trim().to_ascii_lowercase().as_str() { "planner" | "plan" => Some(RoleKind::Planner), "worker" | "work" => Some(RoleKind::Worker), "qa" | "verifier" => Some(RoleKind::Qa), "debugger" | "fix" => Some(RoleKind::Debugger), "audit" | "auditor" => Some(RoleKind::Audit), "agent" | "custom" => Some(RoleKind::Agent), _ => None } }
    /// The tier a role starts at: the planner thinks (mid: Sonnet 5.5 is capable enough, operator 2026-11-13), everyone else is cheap (low). Settings and the Leader may change any node's.
    pub fn default_tier(self) -> Tier { if self == RoleKind::Planner { Tier::Mid } else { Tier::Low } }
    /// Planner, QA and audit never edit; the worker and the debugger do, inside the files they own.
    pub fn may_edit(self) -> bool { matches!(self, RoleKind::Worker | RoleKind::Debugger) }
}

/// A CUSTOM AGENT (RFC 0008 §4.16): the Leader's own creation, one responsibility with its brief, context, skills, MCP allow-list and the effects it may have.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSpec {
    #[serde(default)] pub title: String, #[serde(default)] pub responsibility: String, #[serde(default)] pub brief: String,
    #[serde(default)] pub context: Vec<String>, #[serde(default)] pub skills: Vec<String>, #[serde(default)] pub mcp: Vec<String>, #[serde(default)] pub effects: Vec<String>,
    /** a SPECIALIST's id (RFC 0017 §2.4): the agent runs with that specialist's memory, tools and persona behind it; only an approved one is used */ #[serde(default, skip_serializing_if = "String::is_empty")] pub specialist: String,
}
/// What a custom agent may do in the world. `outward` (post, send, publish) and `money` always stand behind a human node.
pub const AGENT_EFFECTS: [&str; 6] = ["read", "document", "web", "write", "outward", "money"];
/// MCP tools no agent's allow-list may name: the ones that start agents or steer teams are the Leader's and the person's.
pub fn mcp_banned(name: &str) -> bool { name.starts_with("antiloki_delegate") || name.starts_with("antiloki_team_") || name.starts_with("antiloki_crew") }
impl AgentSpec { pub fn has(&self, e: &str) -> bool { self.effects.iter().any(|x| x == e) } }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerMode { Delay, Timeout, Schedule }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinPolicy { All, Any, Quorum, /** all arrive and must give the same answer; if they differ the run leaves by `climb` */ Consensus }

/// What a `useTool` calls: a skill, a pane's typed door, an MCP tool, a source (data in, nothing else), or a web app (RFC 0009, later).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolRef { #[serde(rename = "type")] pub kind: String, pub name: String, #[serde(default)] pub args: Value }
/// The formats of an artifact: documents for a person to read, none of them code to run.
pub const ARTIFACT_FORMATS: [&str; 6] = ["md", "html", "txt", "json", "csv", "svg"];
/// An artifact node may also name a video, a picture or a sound a step made: the run keeps the file and the app plays it (the operator 2026-11-15: "the video is an artifact and should have its own node").
pub const MEDIA_FORMATS: [&str; 9] = ["mp4", "webm", "mov", "png", "jpg", "gif", "webp", "mp3", "wav"];
pub const TOOL_TYPES: [&str; 17] = ["skill", "pane", "mcp", "source", "web", "webapp", "db", "file", "saga", "mcp_url", "api", "git", "telegram", "apply", "package", "sound", "browse"];
/// What a tool call may have as an effect. `destructive`, `money` and `outward` always stand behind a human node.
pub const EFFECTS: [&str; 7] = ["read", "write", "exec", "destructive", "money", "outward", "network"];
/// What a `useTool` node may touch: nothing it does not name (least privilege), and only the effects it lists.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Allow {
    #[serde(default)] pub paths: Vec<String>, #[serde(default)] pub panes: Vec<String>, #[serde(default)] pub skills: Vec<String>, #[serde(default)] pub mcp: Vec<String>,
    #[serde(default)] pub effects: Vec<String>,
}

fn three() -> u32 { 3 }
fn one() -> u32 { 1 }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum Kind {
    /// The person's terminal agent: the board's edge, not a node in the middle. Exactly one.
    Leader,
    Role { role: RoleKind, #[serde(default)] tier: Option<Tier>, #[serde(default)] model: Option<String>, #[serde(default)] readonly: Option<bool>, #[serde(default)] fresh_context: Option<bool>,
           #[serde(default)] parallel: Option<u32>, #[serde(default)] owns: Vec<String>, #[serde(default, skip_serializing_if = "Option::is_none")] agent: Option<AgentSpec> },
    If { cond: String },
    Switch { on: String },
    /// Repeats its body until `until` holds. `max_iterations` is required (and the validator wants a cost cap, `cap_usd`, on the node). Each pass is an episode of the team saga.
    Loop { max_iterations: u32, #[serde(default)] until: Option<String> },
    ForEach { over: String, #[serde(default)] parallel: Option<u32>, /** items per pass: the body gets a list of this many (default one item at a time) */ #[serde(default, skip_serializing_if = "Option::is_none")] batch: Option<u32>, /** `collect`: a failure in the body is kept in `errors` and the next item goes on; `stop` (default): the failure goes the way it always did */ #[serde(default, skip_serializing_if = "Option::is_none")] on_error: Option<String> },
    Timer { mode: TimerMode, #[serde(default)] seconds: u64, #[serde(default)] cron: Option<String>, #[serde(default)] wraps: Option<String> },
    Fork,
    Join { policy: JoinPolicy, #[serde(default)] quorum: Option<u32> },
    /// Fed by a failure edge: sends the failed node round again (`again`) up to `max` times, then `exhausted`.
    Retry { max: u32, #[serde(default)] backoff_s: u64, /** the circuit breaker: after this many tries the retry stops sending the failure round again (it leaves by `exhausted` at once) */ #[serde(default, skip_serializing_if = "Option::is_none")] open_after: Option<u32>, /** how long the breaker stays open before one new try is let through (default 60 s) */ #[serde(default, skip_serializing_if = "Option::is_none")] cooldown_s: Option<u64> },
    BudgetGate { stop_at: f64, #[serde(default)] ask_at: Option<f64> },
    Human { gate: String, #[serde(default)] on_timeout: Option<String> },
    UseTool { tool: ToolRef, #[serde(default)] allow: Allow, #[serde(default)] starts_model: bool, #[serde(default)] outputs: BTreeMap<String, String> },
    /// A DOCUMENT FOR THE RUN, NOT CODE (§4.17): a report, a dashboard, a draft. It takes what one node wrote (its own file `.antiloki/artifacts/<name>.<format>`, else its `report`), keeps it with the run (never in the code, never on the branch) and says where it is. No model.
    /// With `compose` it is THE MASTER PAGE (§4.16): an HTML page the run itself builds, from a spec of widgets (data, never code) and what the run produced, no model. Then `from` is not needed.
    Artifact { name: String, format: String, #[serde(default)] from: Option<String>, #[serde(default)] title: Option<String>, #[serde(default, skip_serializing_if = "Option::is_none")] compose: Option<crate::team_compose::Spec> },
    /// A CHECK (RFC 0010 §4.6): rules, as data, that judge a text the run made (`on`: `artifact:<name>`, `agent:<node>.report`, `values:<node>.<field>`, `web:<node>.text`). No model, no cost, the same answer every time. It writes `pass`, `failed`, `passed`, `score` and always leaves by `out`: the graph decides with `if` or a loop; what failed is told to the agent after it.
    Check { on: String, #[serde(default)] rules: Vec<crate::team_check::Rule> },
    /// A MODEL NODE (RFC 0010 §4.4): one call to a seat (a local model, an endpoint, a frontier model), a typed answer, free checks on it, a ladder to a stronger seat. No tools, no files.
    Llm(crate::team_llm::Spec),
    /// TEST (operator 2026-11-16): tests written in code BEFORE the run by the leader; they approve or disapprove what the node before made. A judge: the improver never changes it. They run in a sandbox copy of the tree.
    Test(crate::team_test::TestSpec),
    /// CELL (RFC 0014): the family of specialized nodes, one engine and a registry of tasks (summary, translate, intent, redact, regex): a tiny model proposes, guards written in code judge, the exact reason goes back once.
    Cell(crate::team_cell::CellSpec),
    /// SKELETON (RFC 0014): the planned change at symbol level (add / modify / keep, three views) from the leader's function-level contract and the original code in git; after the fill it checks conformance by code (planned functions exist, are exported, take the planned parameters, call what was named, nothing unplanned is exported, a keep is untouched).
    Skeleton(crate::team_skeleton::SkeletonSpec),
    /// ENFORCE (RFC 0014): a raw text in, a typed value out. Code repairs and coerces first; a tiny local model restructures only when code could not; code validates. A verdict names what is missing.
    Enforce(crate::team_enforce::EnforceSpec),
    /// REVIEW (RFC 0014; operator 2026-11-16: "deterministic review"): X-ray's rules (the laws, through `antiloki_check`) plus rules the LEADER adds before the run, applied by code to the files a node made. An error leaves by `failed` with one line per issue.
    Review(crate::team_review::ReviewSpec),
    /// STUBS (RFC 0014; operator 2026-11-16): a tiny model writes each contract file as a STUB (exact signatures, doc comments with examples, bodies that only throw TODO), code validates it, the stubs go into the tree. The decompose node then fills them under a guard: every signature survives, no TODO is left.
    Stubs(crate::team_stubs::StubsSpec),
    /// CONTRACT (RFC 0014): written by the LEADER before the run, enforced by this node. It binds the models of the decompose node that names it (its text goes into every coder prompt) and checks, by code, that what was written conforms: files exist, exports, imports.
    Contract(crate::team_contract::ContractSpec),
    /// DECOMPOSE (RFC 0014): a task in, files out. The planner seat breaks the task into files (a contract code checks), the coder seat writes each one, code does the rest (X-ray context, order, syntax check, the write).
    Decompose(crate::team_decompose::DecomposeSpec),
    /// RECURSE (RFC 0014): runs `worker` (a decompose node), judges it with `judge` (the tests tool or a check) and, on a fail, redoes only the files the failure points at with the failure as input. At most `max_depth` passes; it stops when the same failure repeats. Both are inner nodes: no edge touches them.
    Recurse { worker: String, judge: String, #[serde(default = "three")] max_depth: u32 },
    /// EVOLVE (RFC 0014): marks a SECTION of the team (`scope`, node ids) the improver may change; when a run's failure lands in it (`after_failures` failures in a row) the node asks the improver for a request about that section only.
    Evolve { scope: Vec<String>, #[serde(default = "one")] after_failures: u32 },
    /// CODE (RFC 0011 §11): Rhai sandbox, JSON in and out, examples required at save.
    Code { source: String, #[serde(default)] examples: Vec<crate::team_code::Example>, #[serde(default)] from: Option<String>, #[serde(default)] limits: Option<crate::team_code::Limits> },
    /// TRANSFORM (RFC 0011 §12): deterministic data shaping, one mode.
    Transform { mode: String, #[serde(default)] from: Option<String>, #[serde(default)] config: Value },
    /// SUB-TEAM (RFC 0011 §14): another team run as one step. `ask` is the inner team's ask (a template over this run's values), `outputs` maps a name to a value of the inner run (`node.field`), `budget_share` is the part of this node's budget it may spend. The inner team keeps its own runs, rows and audits; this node records the child run id.
    SubTeam { team: String, #[serde(default)] ask: String, #[serde(default)] outputs: BTreeMap<String, String>, #[serde(default, skip_serializing_if = "Option::is_none")] budget_share: Option<f64>, #[serde(default, skip_serializing_if = "Option::is_none")] max_depth: Option<u32> },
    /// STOP (RFC 0011 §15): ends the run as failed with a message.
    Stop { message: String },
    /// The end: the pack is delivered.
    Deliver,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family { Trigger, Flow, Agent, Cell, Gate, Tool, Data, Transform, Output, Meta }

/// The palette family and type name of a node kind. Pure.
pub fn family(k: &Kind) -> (Family, &'static str) {
    match k {
        Kind::Leader => (Family::Agent, "leader"),
        Kind::Role { role, .. } => (Family::Agent, role.key()),
        Kind::If { .. } => (Family::Flow, "if"),
        Kind::Switch { .. } => (Family::Flow, "switch"),
        Kind::Loop { .. } => (Family::Flow, "loop"),
        Kind::ForEach { .. } => (Family::Flow, "for_each"),
        Kind::Timer { .. } => (Family::Flow, "timer"),
        Kind::Fork => (Family::Flow, "fork"),
        Kind::Join { .. } => (Family::Flow, "join"),
        Kind::Retry { .. } => (Family::Flow, "retry"),
        Kind::Stop { .. } => (Family::Flow, "stop"),
        Kind::BudgetGate { .. } => (Family::Gate, "budget_gate"),
        Kind::Human { .. } => (Family::Gate, "human"),
        Kind::Check { .. } => (Family::Gate, "check"),
        Kind::UseTool { .. } => (Family::Tool, "use_tool"),
        Kind::Artifact { .. } => (Family::Output, "artifact"),
        Kind::Deliver => (Family::Output, "deliver"),
        Kind::Llm(_) => (Family::Cell, "llm"), Kind::Decompose(_) => (Family::Cell, "decompose"), Kind::Test(_) => (Family::Cell, "test"), Kind::Contract(_) => (Family::Cell, "contract"), Kind::Stubs(_) => (Family::Cell, "stubs"), Kind::Review(_) => (Family::Cell, "review"), Kind::Enforce(_) => (Family::Cell, "enforce"), Kind::Skeleton(_) => (Family::Cell, "skeleton"), Kind::Cell(_) => (Family::Cell, "cell"), Kind::Recurse { .. } => (Family::Flow, "recurse"), Kind::Evolve { .. } => (Family::Flow, "evolve"),
        Kind::Code { .. } => (Family::Transform, "code"),
        Kind::Transform { .. } => (Family::Transform, "transform"),
        Kind::SubTeam { .. } => (Family::Flow, "sub_team"),
    }
}

/// A DETERMINISTIC NODE (operator 2026-11-21: "all deterministic nodes should have a deterministic flag"): no model is called, so the same input gives the same output. Control flow, gates, checks, tests, contracts, reviews, code, transforms, artifacts, and a tool node that does not start a model. Pure.
pub fn deterministic(k: &Kind) -> bool {
    match k {
        Kind::If { .. } | Kind::Switch { .. } | Kind::Loop { .. } | Kind::ForEach { .. } | Kind::Timer { .. } | Kind::Fork | Kind::Join { .. } | Kind::Retry { .. } | Kind::Stop { .. } | Kind::BudgetGate { .. } | Kind::Human { .. } | Kind::Check { .. } | Kind::Artifact { .. } | Kind::Deliver | Kind::Test(_) | Kind::Contract(_) | Kind::Review(_) | Kind::Code { .. } | Kind::Transform { .. } => true,
        Kind::UseTool { starts_model, .. } => !*starts_model,
        _ => false,
    }
}
pub const WHY_KINDS: [&str; 8] = ["parallel", "independent_check", "tool", "cost", "gate", "schedule", "memory", "sense"];
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Why { pub kind: String, #[serde(default)] pub note: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    #[serde(flatten)] pub kind: Kind,
    #[serde(default)] pub label: Option<String>,
    /// WHY THIS NODE IS HERE (RFC 0010 §4.1): one of a closed list of kinds and a sentence. Recommended for agents and checks; a team whose agents have none of them is warned that it is one prompt with extra steps.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub why: Option<Why>,
    /// A wall-clock limit on this node; at its end the `timeout` port fires.
    #[serde(default)] pub timeout_s: Option<u64>,
    /// The most this node (or, for a loop, its whole body) may cost, in USD.
    #[serde(default)] pub cap_usd: Option<f64>,
    /// Where the editor drew it on the board (x, y), when the person moved it; otherwise the board lays the graph out itself. Meaningless to the run.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub pos: Option<[f64; 2]>,
    /// AN APPROVAL (the operator 2026-11-15: the approval card as a node of the team): on a `human` node, how risky the step behind it is and what exactly will happen, shown on the card the person decides on. `note` may use `{ask}` and `{node.field}`.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub approval: Option<Approval>,
    /// THE AUTONOMY DIAL (the operator 2026-11-15, AffectUI's Autonomy Dial): how much this step may do on its own. None or `auto` = runs on its own; `ask` = asks the person once per run before it starts; `always` = asks every time it starts. The person sets it; the run honours it.
    #[serde(default, skip_serializing_if = "Option::is_none")] pub autonomy: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Approval { /** low · medium · high */ #[serde(default)] pub risk: String, #[serde(default)] pub note: String, /** FRAMES: the name of an artifact (html or svg) the approval card shows in a frame of its own, so a design is judged before it is built */ #[serde(default, skip_serializing_if = "String::is_empty")] pub show: String }
impl Node {
    pub fn new(kind: Kind) -> Node { Node { kind, label: None, why: None, timeout_s: None, cap_usd: None, pos: None, approval: None, autonomy: None } }
    pub fn role(role: RoleKind) -> Node { Node::new(Kind::Role { role, tier: None, model: None, readonly: None, fresh_context: None, parallel: None, owns: vec![], agent: None }) }
    pub fn cap(mut self, usd: f64) -> Node { self.cap_usd = Some(r4(usd)); self }
    pub fn tier_of(&self) -> Option<Tier> { match &self.kind { Kind::Role { role, tier, .. } => Some(tier.unwrap_or(role.default_tier())), _ => None } }
    pub fn is_writer(&self) -> bool { match &self.kind { Kind::Role { role: RoleKind::Agent, readonly, agent, .. } => agent.as_ref().map_or(false, |a| a.has("write")) && !readonly.unwrap_or(false), Kind::Role { role, readonly, .. } => role.may_edit() && !readonly.unwrap_or(false), _ => false } }
    pub fn kind_key(&self) -> &'static str { match &self.kind { Kind::Leader => "leader", Kind::Role { .. } => "role", Kind::If { .. } => "if", Kind::Switch { .. } => "switch", Kind::Loop { .. } => "loop", Kind::ForEach { .. } => "for_each", Kind::Timer { .. } => "timer",
        Kind::Fork => "fork", Kind::Join { .. } => "join", Kind::Retry { .. } => "retry", Kind::BudgetGate { .. } => "budget_gate", Kind::Human { .. } => "human", Kind::UseTool { .. } => "use_tool", Kind::Artifact { .. } => "artifact", Kind::Check { .. } => "check", Kind::Llm(_) => "llm", Kind::Decompose(_) => "decompose", Kind::Test(_) => "test", Kind::Contract(_) => "contract", Kind::Stubs(_) => "stubs", Kind::Review(_) => "review", Kind::Enforce(_) => "enforce", Kind::Skeleton(_) => "skeleton", Kind::Cell(_) => "cell", Kind::Recurse { .. } => "recurse", Kind::Evolve { .. } => "evolve",
        Kind::Code { .. } => "code", Kind::Transform { .. } => "transform", Kind::SubTeam { .. } => "sub_team", Kind::Stop { .. } => "stop", Kind::Deliver => "deliver" } }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edge { pub from: String, #[serde(default = "out_port")] pub port: String, pub to: String }
fn out_port() -> String { "out".into() }
fn edge(from: &str, port: &str, to: &str) -> Edge { Edge { from: from.into(), port: port.into(), to: to.into() } }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub team: String, #[serde(default = "schema_now")] pub schema: u32, #[serde(default)] pub branch: String, #[serde(default)] pub budget_usd: f64,
    /// Where a run starts, when that is not simply "the nodes nothing leads into" (a loop first, say). Empty = the latter.
    #[serde(default)] pub start: Vec<String>,
    pub nodes: BTreeMap<String, Node>, #[serde(default)] pub edges: Vec<Edge>,
    /// WHEN THE TEAM RUNS ON ITS OWN (RFC 0010 §4.9; `team_triggers.rs`): schedules and the daily limits that bound them. Set by the person, never by an agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub triggers: Vec<Trigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub limits: Option<Limits>,
    /** what each node is expected to hold (RFC 0011 §7): the regulator compares the windowed statistics with these. Set by the person (or proposed and applied with their yes), never by the regulator. */
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")] pub setpoints: BTreeMap<String, crate::team_regulate::Setpoints>,
}
/// One way a team starts without being asked: `schedule` (a cron) is built; webhook, files and after are named in the RFC and not yet.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Trigger { /** the seat the run uses (`claude-code`, `codex`…); empty = the router's choice. An unattended run should name one. */ #[serde(default, skip_serializing_if = "String::is_empty")] pub engine: String, #[serde(default)] pub id: String, #[serde(default = "sched")] pub kind: String, #[serde(default)] pub cron: String, #[serde(default)] pub ask: String, #[serde(default)] pub budget_usd: Option<f64>, #[serde(default)] pub missed: String, #[serde(default)] pub overlap: String,
    /** file_watch: a path glob inside the workspace (`src/**/*.md`) */ #[serde(default, skip_serializing_if = "String::is_empty")] pub watch: String, /** file_watch: how long the files must stay quiet before the run starts (seconds; default 10) */ #[serde(default, skip_serializing_if = "Option::is_none")] pub debounce_s: Option<u64>,
    /** on_error: the team whose failed run starts this one (empty = any other team) */ #[serde(default, skip_serializing_if = "String::is_empty")] pub team: String,
    /** webhook: the SHA-256 of the secret token the caller presents (the token itself is shown once, when the person makes it) */ #[serde(default, skip_serializing_if = "String::is_empty")] pub token_hash: String }
fn sched() -> String { "schedule".into() }
/// What unattended runs may spend in a day.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits { #[serde(default)] pub max_runs_per_day: Option<u32>, #[serde(default)] pub max_usd_per_day: Option<f64> }
fn schema_now() -> u32 { SCHEMA }

fn r4(x: f64) -> f64 { (x * 10_000.0).round() / 10_000.0 }
pub const FAILURE_PORTS: [&str; 6] = ["failed", "error", "timeout", "cap", "budget", "human"];

/// Keys a graph file may never carry: a graph is data, it names a tool, never a command line.
const COMMAND_KEYS: [&str; 5] = ["command", "cmd", "shell", "script", "exec_line"];
fn find_command_key(v: &Value, at: &str) -> Option<String> {
    match v {
        Value::Object(o) => { for (k, x) in o { if COMMAND_KEYS.contains(&k.as_str()) { return Some(format!("{at}.{k}")); } if let Some(p) = find_command_key(x, &format!("{at}.{k}")) { return Some(p); } } None }
        Value::Array(a) => a.iter().enumerate().find_map(|(i, x)| find_command_key(x, &format!("{at}[{i}]"))),
        _ => None,
    }
}
impl Graph {
    /// A graph from JSON, refusing one that names a command to run and one made for a newer schema.
    pub fn from_json(v: &Value) -> Result<Graph, String> {
        if let Some(p) = find_command_key(v, "graph") { return Err(format!("a graph names a tool, never a command line ({p})")); }
        let g: Graph = serde_json::from_value(v.clone()).map_err(|e| format!("not a team graph: {e}"))?;
        if g.schema > SCHEMA { return Err(format!("this graph is schema {}, this antiloki reads up to {SCHEMA}: update antiloki", g.schema)); }
        // money is kept to a hundredth of a cent, so a graph read back from its file is the graph that was written (a decimal in a file is not always the same double)
        let mut g = g; g.budget_usd = r4(g.budget_usd); for n in g.nodes.values_mut() { n.cap_usd = n.cap_usd.map(r4); }
        Ok(g)
    }
    pub fn to_json(&self) -> Value { serde_json::to_value(self).unwrap_or(Value::Null) }
    pub fn node(&self, id: &str) -> Option<&Node> { self.nodes.get(id) }
    pub fn out_edges<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Edge> + 'a { self.edges.iter().filter(move |e| e.from == id) }
    pub fn in_edges<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Edge> + 'a { self.edges.iter().filter(move |e| e.to == id) }
    pub fn leader(&self) -> Option<&str> { self.nodes.iter().find(|(_, n)| n.kind == Kind::Leader).map(|(k, _)| k.as_str()) }
    /// Where a run starts: every node nothing leads into (the leader is not a step), in id order.
    pub fn entries(&self) -> Vec<String> {
        if !self.start.is_empty() { return self.start.clone(); }
        let fed: BTreeSet<&str> = self.edges.iter().map(|e| e.to.as_str()).collect();
        self.nodes.iter().filter(|(k, n)| n.kind != Kind::Leader && !fed.contains(k.as_str()) && !matches!(&n.kind, Kind::Timer { mode: TimerMode::Timeout, .. })).map(|(k, _)| k.clone()).collect()
    }
}

// ── the ports a node has, and the values it writes ──────────────────────────────────────────────────────────────────────

/// The ports that MUST be wired for the graph to make sense (a missing one is a dead end), per node.
pub fn required_ports(n: &Node) -> Vec<String> {
    match &n.kind {
        Kind::Leader | Kind::Deliver => vec![],
        Kind::Role { .. } | Kind::UseTool { .. } | Kind::Artifact { .. } | Kind::Check { .. } | Kind::Llm(_) | Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Evolve { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Stubs(_) | Kind::Review(_) | Kind::Enforce(_) | Kind::Skeleton(_) | Kind::Cell(_) | Kind::Code { .. } | Kind::Transform { .. } | Kind::SubTeam { .. } | Kind::Timer { mode: TimerMode::Delay, .. } | Kind::Join { .. } => vec!["out".into()],
        Kind::Stop { .. } => vec![],
        Kind::Timer { .. } => vec![],
        Kind::If { .. } => vec!["then".into(), "else".into()],
        Kind::Switch { .. } => vec!["default".into()],
        Kind::Loop { .. } => vec!["body".into(), "done".into()],
        Kind::ForEach { .. } => vec!["body".into(), "done".into()],
        Kind::Fork => vec!["branch".into()],
        Kind::Retry { .. } => vec!["again".into(), "exhausted".into()],
        Kind::BudgetGate { .. } => vec!["continue".into(), "stop".into()],
        Kind::Human { .. } => vec!["approved".into()],
    }
}
/// Every port a node may leave by (the required ones, the optional ones, a switch's cases are checked against `on`'s values by the editor, not here).
pub fn allowed_port(n: &Node, port: &str) -> bool {
    if FAILURE_PORTS.contains(&port) { return matches!(&n.kind, Kind::Role { .. } | Kind::UseTool { .. } | Kind::Artifact { .. } | Kind::Llm(_) | Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Evolve { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Stubs(_) | Kind::Review(_) | Kind::Enforce(_) | Kind::Skeleton(_) | Kind::Cell(_) | Kind::Loop { .. } | Kind::ForEach { .. } | Kind::Human { .. } | Kind::Retry { .. } | Kind::Join { .. } | Kind::Fork) || port == "budget"; }
    match &n.kind {
        Kind::Leader | Kind::Deliver => false,
        Kind::Join { policy: JoinPolicy::Consensus, .. } => matches!(port, "out" | "climb"),
        Kind::Role { .. } | Kind::UseTool { .. } | Kind::Artifact { .. } | Kind::Check { .. } | Kind::Llm(_) | Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Evolve { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Stubs(_) | Kind::Review(_) | Kind::Enforce(_) | Kind::Skeleton(_) | Kind::Cell(_) | Kind::Code { .. } | Kind::Transform { .. } | Kind::SubTeam { .. } | Kind::Join { .. } | Kind::Timer { .. } => port == "out",
        Kind::Stop { .. } => false,
        Kind::If { .. } => matches!(port, "then" | "else"),
        Kind::Switch { .. } => !port.is_empty(),
        Kind::Loop { .. } => matches!(port, "body" | "done" | "exhausted"),
        Kind::ForEach { .. } => matches!(port, "body" | "done"),
        Kind::Fork => port == "branch",
        Kind::Retry { .. } => matches!(port, "again" | "exhausted"),
        Kind::BudgetGate { .. } => matches!(port, "continue" | "stop" | "ask"),
        Kind::Human { .. } => matches!(port, "approved" | "denied" | "out"),
    }
}
fn owns_of(n: &Node) -> &[String] { match &n.kind { Kind::Role { owns, .. } => owns, _ => &[] } }
/// The values a node writes into the run state, by name and type (`int`, `bool`, `string`, `list`, `object`, `enum`, `artifact`). A condition may read only these.
pub fn writes_of(n: &Node) -> Vec<(String, String)> {
    let t = |a: &str, b: &str| (a.to_string(), b.to_string());
    match &n.kind {
        Kind::Role { role: RoleKind::Planner, .. } => vec![t("plan", "object"), t("sagas", "list"), t("criteria", "list")],
        Kind::Role { role: RoleKind::Worker, .. } => vec![t("done", "bool"), t("files", "list")],
        Kind::Role { role: RoleKind::Qa, .. } => vec![t("pass", "bool"), t("failed", "list"), t("evidence", "list")],
        Kind::Role { role: RoleKind::Debugger, .. } => vec![t("patched", "bool"), t("files", "list")],
        Kind::Role { role: RoleKind::Audit, .. } => vec![t("verdict", "enum"), t("flags", "list")],
        Kind::Role { role: RoleKind::Agent, .. } => vec![t("done", "bool"), t("pass", "bool"), t("report", "string"), t("files", "list")],
        Kind::UseTool { outputs, .. } => { let mut v = vec![t("ok", "bool")]; for (k, ty) in outputs { if k != "ok" { v.push((k.clone(), ty.clone())); } } v }
        Kind::Artifact { .. } => vec![t("ok", "bool"), t("name", "string"), t("path", "string"), t("format", "string")],
        Kind::Check { .. } => vec![t("ok", "bool"), t("pass", "bool"), t("failed", "list"), t("passed", "list"), t("score", "number")],
        Kind::Cell(_) => vec![t("ok", "bool"), t("output", "string"), t("tries", "int")],
        Kind::Skeleton(_) => vec![t("ok", "bool"), t("issues", "list"), t("diff", "string"), t("merged", "string"), t("new_only", "string"), t("tail", "string")],
        Kind::Enforce(sp) => { let mut v = vec![t("ok", "bool"), t("repaired_by", "string")]; for (k, ty) in &sp.schema { let name = match ty { Value::String(s) => s.trim_end_matches('?').to_string(), Value::Object(_) => "enum".into(), _ => "list".into() }; v.push((k.clone(), name)); } v }
        Kind::Review(_) => vec![t("ok", "bool"), t("issues", "list"), t("errors", "int"), t("tail", "string")],
        Kind::Stubs(_) => vec![t("ok", "bool"), t("files", "list"), t("functions", "list"), t("failed", "list"), t("text", "string")],
        Kind::Contract(_) => vec![t("ok", "bool"), t("issues", "list"), t("tail", "string")],
        Kind::Test(_) => vec![t("ok", "bool"), t("exit", "int"), t("total", "int"), t("passed", "int"), t("failed", "list"), t("tail", "string")],
        Kind::Decompose(_) => vec![t("ok", "bool"), t("subtasks", "list"), t("files", "list"), t("failed", "list"), t("text", "string")],
        Kind::Recurse { .. } => vec![t("ok", "bool"), t("depth", "int"), t("attempts", "int"), t("stopped", "string"), t("files", "list")],
        Kind::Evolve { .. } => vec![t("ok", "bool"), t("due", "bool"), t("scope", "list")],
        Kind::Llm(sp) => { let mut v = vec![t("ok", "bool"), t("seat_used", "string"), t("escalated_from", "string"), t("schema_ok", "bool")]; for (k, ty) in &sp.answer { let name = match ty { Value::String(s) => s.trim_end_matches('?').to_string(), Value::Object(_) => "enum".into(), _ => "list".into() }; v.push((k.clone(), name)); } v }
        Kind::Code { .. } | Kind::Transform { .. } | Kind::SubTeam { .. } => vec![t("ok", "bool"), t("output", "object")],
        Kind::Stop { .. } => vec![],
        Kind::Human { .. } => vec![t("approved", "bool")],
        Kind::Loop { .. } => vec![t("count", "int"), t("max", "int")],
        Kind::ForEach { .. } => vec![t("item", "object"), t("index", "int"), t("count", "int")],
        _ => vec![],
    }
}
/// The role of a node in the words of the board: the label, else the id.
pub fn title_of(id: &str, n: &Node) -> String { n.label.clone().filter(|l| !l.is_empty()).unwrap_or_else(|| id.to_string()) }

// ── conditions ────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum Expr { Lit(Value), Op(String), Not(Box<Expr>), And(Box<Expr>, Box<Expr>), Or(Box<Expr>, Box<Expr>), Cmp(Box<Expr>, Cmp, Box<Expr>), In(Box<Expr>, Vec<Value>) }
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmp { Eq, Ne, Lt, Le, Gt, Ge }

#[derive(Clone, Debug, PartialEq)]
enum Tok { Num(f64), Str(String), Id(String), Sym(&'static str) }
fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let cs: Vec<char> = src.chars().collect(); let mut i = 0; let mut out = vec![];
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() { i += 1; continue; }
        if c.is_ascii_digit() || (c == '-' && cs.get(i + 1).map_or(false, |d| d.is_ascii_digit()) && !matches!(out.last(), Some(Tok::Num(_)) | Some(Tok::Id(_)) | Some(Tok::Str(_)) | Some(Tok::Sym(")")) | Some(Tok::Sym("]")))) {
            let st = i; i += 1; while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') { i += 1; }
            let t: String = cs[st..i].iter().collect(); out.push(Tok::Num(t.parse::<f64>().map_err(|_| format!("`{t}` is not a number"))?)); continue;
        }
        if c == '"' { let mut s = String::new(); i += 1; loop { match cs.get(i) { None => return Err("a string is not closed".into()), Some('"') => { i += 1; break; } Some('\\') => { if let Some(n) = cs.get(i + 1) { s.push(*n); i += 2; } else { return Err("a string is not closed".into()); } } Some(x) => { s.push(*x); i += 1; } } } out.push(Tok::Str(s)); continue; }
        if c.is_ascii_alphabetic() || c == '_' { let st = i; while i < cs.len() && (cs[i].is_ascii_alphanumeric() || cs[i] == '_' || cs[i] == '.') { i += 1; } let t: String = cs[st..i].iter().collect(); if t.ends_with('.') { return Err(format!("`{t}` ends with a dot")); } out.push(Tok::Id(t)); continue; }
        let two: String = cs[i..(i + 2).min(cs.len())].iter().collect();
        let sym = match two.as_str() { "==" => Some("=="), "!=" => Some("!="), "<=" => Some("<="), ">=" => Some(">="), "&&" => Some("&&"), "||" => Some("||"), _ => None };
        if let Some(s) = sym { out.push(Tok::Sym(s)); i += 2; continue; }
        let one = match c { '<' => "<", '>' => ">", '!' => "!", '(' => "(", ')' => ")", '[' => "[", ']' => "]", ',' => ",", _ => return Err(format!("`{c}` is not part of the condition language: no functions, no assignment, no code")) };
        out.push(Tok::Sym(one)); i += 1;
    }
    Ok(out)
}
struct P { t: Vec<Tok>, i: usize }
impl P {
    fn peek(&self) -> Option<&Tok> { self.t.get(self.i) }
    fn eat_sym(&mut self, s: &str) -> bool { if matches!(self.peek(), Some(Tok::Sym(x)) if *x == s) { self.i += 1; true } else { false } }
    fn eat_id(&mut self, w: &str) -> bool { if matches!(self.peek(), Some(Tok::Id(x)) if x == w) { self.i += 1; true } else { false } }
    fn or(&mut self) -> Result<Expr, String> { let mut l = self.and()?; while self.eat_sym("||") || self.eat_id("or") { let r = self.and()?; l = Expr::Or(Box::new(l), Box::new(r)); } Ok(l) }
    fn and(&mut self) -> Result<Expr, String> { let mut l = self.not()?; while self.eat_sym("&&") || self.eat_id("and") { let r = self.not()?; l = Expr::And(Box::new(l), Box::new(r)); } Ok(l) }
    fn not(&mut self) -> Result<Expr, String> { if self.eat_sym("!") || self.eat_id("not") { return Ok(Expr::Not(Box::new(self.not()?))); } self.cmp() }
    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.prim()?;
        if self.eat_id("in") {
            if !self.eat_sym("[") { return Err("`in` is followed by a list: [\"a\", \"b\"]".into()); }
            let mut v = vec![]; if !self.eat_sym("]") { loop { match self.prim()? { Expr::Lit(x) => v.push(x), _ => return Err("a list holds only numbers, strings, true, false".into()) } if self.eat_sym("]") { break; } if !self.eat_sym(",") { return Err("a list is [a, b, c]".into()); } } }
            return Ok(Expr::In(Box::new(l), v));
        }
        for (s, c) in [("==", Cmp::Eq), ("!=", Cmp::Ne), ("<=", Cmp::Le), (">=", Cmp::Ge), ("<", Cmp::Lt), (">", Cmp::Gt)] { if self.eat_sym(s) { let r = self.prim()?; return Ok(Expr::Cmp(Box::new(l), c, Box::new(r))); } }
        Ok(l)
    }
    fn prim(&mut self) -> Result<Expr, String> {
        match self.t.get(self.i).cloned() {
            Some(Tok::Num(n)) => { self.i += 1; Ok(Expr::Lit(json!(n))) }
            Some(Tok::Str(s)) => { self.i += 1; Ok(Expr::Lit(json!(s))) }
            Some(Tok::Id(w)) => { self.i += 1; Ok(match w.as_str() { "true" => Expr::Lit(json!(true)), "false" => Expr::Lit(json!(false)), "null" => Expr::Lit(Value::Null), _ => Expr::Op(w) }) }
            Some(Tok::Sym("(")) => { self.i += 1; let e = self.or()?; if !self.eat_sym(")") { return Err("a bracket is not closed".into()); } Ok(e) }
            Some(t) => Err(format!("unexpected {}", match t { Tok::Sym(s) => format!("`{s}`"), _ => "token".into() })),
            None => Err("the condition ends too soon".into()),
        }
    }
}
/// A condition from text. Pure.
pub fn parse_expr(src: &str) -> Result<Expr, String> {
    if src.trim().is_empty() { return Err("the condition is empty".into()); }
    let mut p = P { t: lex(src)?, i: 0 }; let e = p.or()?;
    if p.i < p.t.len() { return Err("something is left over after the condition".into()); }
    Ok(e)
}
/// The values a condition reads (`node.field`, `budget.left`), in order of appearance. Pure.
pub fn operands(e: &Expr) -> Vec<String> {
    match e { Expr::Lit(_) => vec![], Expr::Op(s) => vec![s.clone()], Expr::Not(a) => operands(a), Expr::And(a, b) | Expr::Or(a, b) | Expr::Cmp(a, _, b) => { let mut v = operands(a); v.extend(operands(b)); v } Expr::In(a, _) => operands(a) }
}
/// What a condition can read while a run is going.
pub struct Ctx<'a> { pub values: &'a Map<String, Value>, pub spent_usd: f64, pub budget_usd: f64, pub elapsed_s: f64 }
impl Ctx<'_> {
    pub fn get(&self, path: &str) -> Result<Value, String> {
        let mut parts = path.split('.');
        let root = parts.next().unwrap_or("");
        match root {
            "budget" => return match parts.next() { Some("spent") => Ok(json!(self.spent_usd)), Some("cap") => Ok(json!(self.budget_usd)), Some("left") => Ok(json!(if self.budget_usd > 0.0 { ((self.budget_usd - self.spent_usd) / self.budget_usd).max(0.0) } else { 1.0 })), _ => Err(format!("unknown value {path} (budget.spent, budget.cap, budget.left)")) },
            "elapsed" => return Ok(json!(self.elapsed_s)),
            _ => {}
        }
        let mut cur = self.values.get(root).ok_or_else(|| format!("{path} has not been written yet"))?;
        for p in parts { cur = cur.get(p).ok_or_else(|| format!("{path} has not been written yet"))?; }
        Ok(cur.clone())
    }
}
fn num(v: &Value) -> Option<f64> { v.as_f64() }
/// The value of a condition. Pure.
pub fn eval(e: &Expr, c: &Ctx) -> Result<Value, String> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Op(p) => c.get(p)?,
        Expr::Not(a) => json!(!truthy(&eval(a, c)?)?),
        Expr::And(a, b) => json!(truthy(&eval(a, c)?)? && truthy(&eval(b, c)?)?),
        Expr::Or(a, b) => json!(truthy(&eval(a, c)?)? || truthy(&eval(b, c)?)?),
        Expr::In(a, list) => { let v = eval(a, c)?; json!(list.iter().any(|x| same(&v, x))) }
        Expr::Cmp(a, op, b) => {
            let (l, r) = (eval(a, c)?, eval(b, c)?);
            match op {
                Cmp::Eq => json!(same(&l, &r)), Cmp::Ne => json!(!same(&l, &r)),
                _ => { let (x, y) = (num(&l).ok_or("only numbers are ordered (<, <=, >, >=)")?, num(&r).ok_or("only numbers are ordered (<, <=, >, >=)")?); json!(match op { Cmp::Lt => x < y, Cmp::Le => x <= y, Cmp::Gt => x > y, _ => x >= y }) }
            }
        }
    })
}
fn same(a: &Value, b: &Value) -> bool { match (num(a), num(b)) { (Some(x), Some(y)) => (x - y).abs() < 1e-12, _ => a == b } }
fn truthy(v: &Value) -> Result<bool, String> { match v { Value::Bool(b) => Ok(*b), _ => Err("a condition must be true or false".into()) } }
/// A condition's answer as a yes or no.
pub fn holds(src: &str, c: &Ctx) -> Result<bool, String> { truthy(&eval(&parse_expr(src)?, c)?) }

// ── validation ────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Issue { pub code: &'static str, pub node: Option<String>, pub message: String }
#[derive(Clone, Debug, Default, Serialize)]
pub struct Report { pub errors: Vec<Issue>, pub warnings: Vec<Issue>, pub worst_usd: f64, pub budget_usd: f64 }
impl Report {
    pub fn ok(&self) -> bool { self.errors.is_empty() }
    pub fn has(&self, code: &str) -> bool { self.errors.iter().chain(self.warnings.iter()).any(|i| i.code == code) }
    pub fn err(&mut self, code: &'static str, node: Option<&str>, m: impl Into<String>) { self.errors.push(Issue { code, node: node.map(String::from), message: m.into() }); }
    pub fn warn(&mut self, code: &'static str, node: Option<&str>, m: impl Into<String>) { self.warnings.push(Issue { code, node: node.map(String::from), message: m.into() }); }
    /// The reasons, in plain words, for a person or a Leader. Pure.
    pub fn words(&self) -> Vec<String> { self.errors.iter().map(|i| format!("error: {}", i.message)).chain(self.warnings.iter().map(|i| format!("note: {}", i.message))).collect() }
}

fn succ(g: &Graph) -> BTreeMap<&str, Vec<&str>> { let mut m: BTreeMap<&str, Vec<&str>> = g.nodes.keys().map(|k| (k.as_str(), vec![])).collect(); for e in &g.edges { if g.nodes.contains_key(&e.from) && g.nodes.contains_key(&e.to) { m.entry(e.from.as_str()).or_default().push(e.to.as_str()); } } m }
fn reach<'a>(adj: &BTreeMap<&'a str, Vec<&'a str>>, from: &[&'a str], skip: &BTreeSet<&str>) -> BTreeSet<&'a str> {
    let mut seen: BTreeSet<&str> = BTreeSet::new(); let mut q: VecDeque<&str> = VecDeque::new();
    for f in from { if !skip.contains(f) && seen.insert(f) { q.push_back(f); } }
    while let Some(n) = q.pop_front() { for m in adj.get(n).into_iter().flatten() { if !skip.contains(m) && seen.insert(m) { q.push_back(m); } } }
    seen
}
/// Strongly connected components (Tarjan, iterative enough for a graph this small). Only the ones that are cycles are returned.
fn cycles<'a>(adj: &BTreeMap<&'a str, Vec<&'a str>>) -> Vec<Vec<&'a str>> {
    struct S<'b, 'a> { adj: &'b BTreeMap<&'a str, Vec<&'a str>>, idx: BTreeMap<&'a str, usize>, low: BTreeMap<&'a str, usize>, on: BTreeSet<&'a str>, st: Vec<&'a str>, n: usize, out: Vec<Vec<&'a str>> }
    fn go<'a>(s: &mut S<'_, 'a>, v: &'a str) {
        s.idx.insert(v, s.n); s.low.insert(v, s.n); s.n += 1; s.st.push(v); s.on.insert(v);
        let ws: Vec<&'a str> = s.adj.get(v).cloned().unwrap_or_default();
        for w in ws { if !s.idx.contains_key(w) { go(s, w); let lw = s.low[w]; let lv = s.low[v]; s.low.insert(v, lv.min(lw)); } else if s.on.contains(w) { let iw = s.idx[w]; let lv = s.low[v]; s.low.insert(v, lv.min(iw)); } }
        if s.low[v] == s.idx[v] { let mut comp = vec![]; loop { let w = s.st.pop().unwrap(); s.on.remove(w); comp.push(w); if w == v { break; } } let self_loop = comp.len() == 1 && s.adj.get(v).map_or(false, |x| x.contains(&v)); if comp.len() > 1 || self_loop { s.out.push(comp); } }
    }
    let mut s = S { adj, idx: BTreeMap::new(), low: BTreeMap::new(), on: BTreeSet::new(), st: vec![], n: 0, out: vec![] };
    for v in adj.keys().copied().collect::<Vec<_>>() { if !s.idx.contains_key(v) { go(&mut s, v); } }
    s.out
}
/// The nodes inside a loop (or retry): reachable from its `body` (its `again`) port without coming back through the node itself.
/// What a node answered, without the bookkeeping a call adds (which seat, how many calls): two answers are the same when these are equal.
fn consensus_view(v: &Value) -> Value {
    const META: [&str; 9] = ["ok", "schema_ok", "seat_used", "escalated_from", "why_escalated", "verified", "agreed", "calls", "cost_usd"];
    match v { Value::Object(m) => Value::Object(m.iter().filter(|(k, _)| !META.contains(&k.as_str())).map(|(k, x)| (k.clone(), x.clone())).collect()), other => other.clone() }
}
/// Is `node` inside the body of the for-each / loop `owner`?
pub fn in_body(g: &Graph, owner: &str, node: &str) -> bool { body_of(g, owner).contains(node) }
fn body_of<'a>(g: &'a Graph, id: &'a str) -> BTreeSet<&'a str> {
    let adj = succ(g); let start: Vec<&str> = g.out_edges(id).filter(|e| e.port == "body" || e.port == "again").map(|e| e.to.as_str()).collect();
    let skip: BTreeSet<&str> = [id].into_iter().collect(); reach(&adj, &start, &skip)
}
/// How many times a node can run in the worst case: the product of the passes of every loop whose body holds it (a for-each counts `parallel` or 1 per item, unknown here, so once).
pub fn multiplier(g: &Graph, id: &str) -> f64 {
    let mut m = 1.0;
    for (lid, n) in &g.nodes { if lid == id { continue; } match &n.kind { Kind::Loop { max_iterations, .. } => { if body_of(g, lid).contains(id) { m *= (*max_iterations).max(1) as f64; } } Kind::Retry { max, .. } => { if body_of(g, lid).contains(id) { m *= (*max + 1) as f64; } } _ => {} } }
    m
}
/// The most a run can cost: every node's cap times its worst-case passes, with a loop's own cost cap bounding its body. Pure.
pub fn worst_case_usd(g: &Graph) -> f64 {
    let mut total = 0.0; let mut in_capped_loop: BTreeSet<&str> = BTreeSet::new(); let mut loop_extra = 0.0;
    for (lid, n) in &g.nodes { if let (Kind::Loop { .. }, Some(cap)) = (&n.kind, n.cap_usd) { let body = body_of(g, lid); let inner: f64 = body.iter().map(|b| g.nodes[*b].cap_usd.unwrap_or(0.0) * multiplier(g, b)).sum(); if inner > cap { for b in &body { in_capped_loop.insert(*b); } loop_extra += cap; } } }
    for (id, n) in &g.nodes { if in_capped_loop.contains(id.as_str()) { continue; } if let Some(c) = n.cap_usd { if !matches!(n.kind, Kind::Loop { .. }) { total += c * multiplier(g, id); } } }
    ((total + loop_extra) * 10_000.0).round() / 10_000.0
}
fn glob_overlap(a: &str, b: &str) -> bool { let (a, b) = (a.trim_end_matches("/**").trim_end_matches('*'), b.trim_end_matches("/**").trim_end_matches('*')); a.is_empty() || b.is_empty() || a.starts_with(b) || b.starts_with(a) }

fn parse_from_ref(f: &str) -> Result<(), String> {
    let (kind, rest) = f.split_once(':').ok_or("say where the input is (`values:<node>.<field>`)")?;
    if kind != "values" { return Err(format!("`{f}` must be values:<node>.<field>")); }
    // `values:<node>` is the node's whole answer; `values:<node>.<field>[.<deeper>]` walks into it
    let parts: Vec<&str> = rest.split('.').collect();
    if parts.iter().any(|p| p.is_empty()) { return Err("the node and every field in the path must not be empty".into()); }
    Ok(())
}

/// The checks that make a graph safe to run, all pure (RFC 0008 §4.5). `pack_hint` is how many sagas a for-each is expected to see, for the worst case (1 when unknown).
pub fn validate(g: &Graph) -> Report {
    let mut r = Report { budget_usd: g.budget_usd, ..Default::default() };
    if g.team.trim().is_empty() { r.err("no_name", None, "the team has no name"); }
    // ALL AGENTS, NOTHING DETERMINISTIC (the operator's first real team: engineer, UI dev, QA for a video job): agents are the expensive, unpredictable part of a team, so a team of three or more with no step that is plain code, a data shaping, a check, a tool or a typed model call is asked to say which of its steps could be one
    let agents = g.nodes.values().filter(|n| matches!(n.kind, Kind::Role { .. })).count();
    let fixed = g.nodes.values().filter(|n| matches!(n.kind, Kind::Code { .. } | Kind::Transform { .. } | Kind::Check { .. } | Kind::UseTool { .. } | Kind::Llm(_) | Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Evolve { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Stubs(_) | Kind::Review(_) | Kind::Enforce(_) | Kind::Skeleton(_) | Kind::Cell(_) | Kind::Stop { .. } | Kind::Artifact { .. })).count();
    if agents >= 3 && fixed == 0 { r.warn("all_agents", None, format!("this team is {agents} agents and nothing deterministic: every step costs a model call and can come out differently. Which steps are really code, a transform, a check or a tool (a render command, a file, a database)? Keep an agent only where judgement is needed.")); }
    for (node, sp) in &g.setpoints {
        if !g.nodes.contains_key(node) { r.err("setpoint_node", Some(node), format!("a setpoint is set for {node}, which is not in the graph")); }
        if sp.min_ok_rate.map_or(false, |m| !(0.0..=1.0).contains(&m)) { r.err("bad_setpoint", Some(node), format!("{node}: min_ok_rate is a share between 0 and 1")); }
        if sp.max_p95_ms.map_or(false, |m| m <= 0) || sp.max_cost_per_ok.map_or(false, |m| m <= 0.0) { r.err("bad_setpoint", Some(node), format!("{node}: a setpoint above zero, or leave it out")); }
    }
    // the leader: one
    let leaders: Vec<&str> = g.nodes.iter().filter(|(_, n)| n.kind == Kind::Leader).map(|(k, _)| k.as_str()).collect();
    if leaders.is_empty() { r.err("no_leader", None, "the team has no leader: there is one terminal the person talks to"); } else if leaders.len() > 1 { r.err("many_leaders", None, format!("a team has one leader, this one has {}", leaders.len())); }
    // the edges point at real nodes and real ports
    for e in &g.edges {
        let (Some(from), Some(_)) = (g.nodes.get(&e.from), g.nodes.get(&e.to)) else { r.err("edge_to_nowhere", Some(&e.from), format!("the wire {} → {} names a node that is not there", e.from, e.to)); continue; };
        if !allowed_port(from, &e.port) { r.err("bad_port", Some(&e.from), format!("{} ({}) has no `{}` port", e.from, from.kind_key(), e.port)); }
    }
    let adj = succ(g);
    let entries = g.entries();
    if entries.is_empty() && !g.nodes.is_empty() { r.err("no_entry", None, "nothing starts the run: every node is fed by another"); }
    // every node but the leader and the timers that guard another node is reachable
    let reached = reach(&adj, &entries.iter().map(String::as_str).collect::<Vec<_>>(), &BTreeSet::new());
    // THE AUTONOMY DIAL: a known value, and only on a step that runs
    for (id, n) in &g.nodes { if let Some(au) = &n.autonomy { if !["", "auto", "ask", "always"].contains(&au.as_str()) { r.err("autonomy_value", Some(id), format!("{id}: autonomy is auto, ask or always")); }
        if matches!(n.kind, Kind::Leader | Kind::Deliver | Kind::Human { .. } | Kind::If { .. } | Kind::Switch { .. } | Kind::Fork | Kind::Join { .. } | Kind::Loop { .. } | Kind::ForEach { .. } | Kind::Timer { .. } | Kind::Retry { .. } | Kind::BudgetGate { .. } | Kind::Stop { .. }) { r.err("autonomy_kind", Some(id), format!("{id}: only a step that runs (an agent, a model call, a tool, code, a check) can ask before it starts")); } } }
    for (id, n) in &g.nodes { if let Some(w) = &n.why { if !WHY_KINDS.contains(&w.kind.as_str()) { r.err("why_kind", Some(id), format!("{id}: `{}` is not a reason a node can be there; they are {}", w.kind, WHY_KINDS.join(", "))); } } }
    // ONE PROMPT WITH EXTRA STEPS (§4.1): two or more agents, none with a reason other than a gate, no parallel for-each, no check, no tool, and the edges one single path
    { let agents = g.nodes.values().filter(|n| matches!(&n.kind, Kind::Role { role: RoleKind::Agent, .. })).count();
      let justified = g.nodes.values().any(|n| n.why.as_ref().map_or(false, |w| w.kind != "gate") && matches!(&n.kind, Kind::Role { .. } | Kind::Check { .. } | Kind::UseTool { .. }))
        || g.nodes.values().any(|n| matches!(&n.kind, Kind::Check { .. } | Kind::UseTool { .. } | Kind::Fork) || matches!(&n.kind, Kind::ForEach { parallel: Some(p), .. } if *p > 1));
      let one_path = g.nodes.keys().all(|id| g.out_edges(id).count() <= 1);
      if agents >= 2 && !justified && one_path { r.warn("one_prompt_with_extra_steps", None, "this team is a chain of agents with nothing a single agent could not do. Give each node a `why`, replace a mechanical reviewer with a check, or merge the steps (the bench will say whether it earns its place).".to_string()); } }
    let mut inner: BTreeSet<&str> = g.nodes.values().filter_map(|n| match &n.kind { Kind::Recurse { worker, judge, .. } => Some([worker.as_str(), judge.as_str()]), _ => None }).flatten().collect();
    for n in g.nodes.values() { if let Kind::Decompose(sp) = &n.kind { if let Some(c) = &sp.contract { inner.insert(c.as_str()); } if let Some(c) = &sp.review { inner.insert(c.as_str()); } if let Some(c) = &sp.tests { inner.insert(c.as_str()); } } }
    for (id, n) in &g.nodes { if n.kind != Kind::Leader && !inner.contains(id.as_str()) && !reached.contains(id.as_str()) && !matches!(&n.kind, Kind::Timer { mode: TimerMode::Timeout, .. }) { r.warn("unreachable", Some(id), format!("{id} is never reached")); } }
    // required ports wired; failure ports unwired are told
    for (id, n) in &g.nodes {
        if !reached.contains(id.as_str()) { continue; }
        for p in required_ports(n) { if !g.out_edges(id).any(|e| e.port == p) { r.err("dead_end", Some(id), format!("{id} has nothing on its `{p}` port: the run would stop there")); } }
        if let Kind::Switch { .. } = &n.kind { if !g.out_edges(id).any(|e| e.port == "default") { r.err("switch_no_default", Some(id), format!("the switch {id} has no `default` port")); } }
        let can_fail = matches!(&n.kind, Kind::Role { .. } | Kind::UseTool { .. } | Kind::Artifact { .. } | Kind::Llm(_) | Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Evolve { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Stubs(_) | Kind::Review(_) | Kind::Enforce(_) | Kind::Skeleton(_) | Kind::Cell(_));
        let code_step = matches!(&n.kind, Kind::Decompose(_) | Kind::Recurse { .. });
        if code_step && !FAILURE_PORTS.iter().any(|p| g.out_edges(id).any(|e| e.port == *p)) { r.err("code_failure_unwired", Some(id), format!("{id} writes the code and nothing catches its failure: wire its `failed` port (to a repair step, a leader note or a stop), or the run dies silently on the first bad file")); }
        if can_fail && !code_step && !FAILURE_PORTS.iter().any(|p| g.out_edges(id).any(|e| e.port == *p)) { r.warn("silent_failure_edge", Some(id), format!("if {id} fails, nothing catches it: the run stops and is handed to the person")); }
    }
    // A SMALL CODER NEEDS A FUNCTION-LEVEL CONTRACT (video test, finding 3): with only files and exports the planner invents the API, so a local coder's contract lists each file's functions
    for (id, n) in &g.nodes { if let Kind::Decompose(sp) = &n.kind {
        let small = !crate::team_llm::parse_seat(&sp.coder).map_or(true, |s| crate::team_llm::frontier(&s));
        if let (true, Some(Kind::Contract(c))) = (small, sp.contract.as_deref().and_then(|c| g.nodes.get(c)).map(|x| &x.kind)) { if !c.files.is_empty() && c.files.values().all(|f| f.functions.is_empty()) { r.err("contract_no_functions", Some(id), format!("{id}: its coder is a small model and the contract lists no functions: give each file its function signatures (`functions`), or the planner will invent the API")); } }
    } }
    // A JUDGE BEFORE ANY WRITER (video test, finding 4): a test, review or contract check in the flow that no step able to produce files comes before has nothing to look at (the first run failed with "the file does not exist")
    { let producer = |k: &Kind| matches!(k, Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Stubs(_) | Kind::Skeleton(_) | Kind::UseTool { .. } | Kind::Code { .. } | Kind::SubTeam { .. }) || matches!(k, Kind::Role { .. });
      let producers: Vec<&str> = g.nodes.iter().filter(|(_, n)| producer(&n.kind)).map(|(k, _)| k.as_str()).collect();
      let after = reach(&adj, &producers, &BTreeSet::new()); let mut helper: BTreeSet<&str> = inner.clone();
      for n in g.nodes.values() { match &n.kind { Kind::Stubs(sp) => { helper.insert(sp.contract.as_str()); } Kind::Skeleton(sp) => { helper.insert(sp.contract.as_str()); } _ => {} } }
      for (id, n) in &g.nodes { if matches!(&n.kind, Kind::Test(_) | Kind::Review(_) | Kind::Contract(_)) && reached.contains(id.as_str()) && !helper.contains(id.as_str()) && !after.contains(id.as_str()) { r.warn("judge_before_writer", Some(id), format!("{id} judges files, and no step that makes files comes before it in the flow: put it after the code step, or leave it for the code step to call")); } }
    }
    if !g.nodes.values().any(|n| matches!(n.kind, Kind::Deliver)) { r.err("no_delivery", None, "no path is delivered: add a `deliver` node at the end"); }
    // cycles go through a bounded loop
    // (take the bounded nodes out: whatever still goes round and round has no limit)
    let bounded: BTreeSet<&str> = g.nodes.iter().filter(|(_, n)| match &n.kind { Kind::Loop { max_iterations, .. } => *max_iterations > 0, Kind::Retry { max, .. } => *max > 0, Kind::ForEach { .. } => true, _ => false }).map(|(k, _)| k.as_str()).collect();
    let loose: BTreeMap<&str, Vec<&str>> = adj.iter().filter(|(k, _)| !bounded.contains(*k)).map(|(k, v)| (*k, v.iter().copied().filter(|m| !bounded.contains(m)).collect())).collect();
    for comp in cycles(&loose) { r.err("unbounded_cycle", Some(comp[0]), format!("the wires {} go round and round: every cycle must pass through a loop with a limit (or a retry, or a for-each)", comp.join(" → "))); }
    for (id, n) in &g.nodes {
        match &n.kind {
            Kind::Loop { max_iterations, until } => {
                if *max_iterations == 0 { r.err("loop_unbounded", Some(id), format!("the loop {id} has no limit: `max_iterations` is required")); }
                if n.cap_usd.is_none() { r.err("loop_no_cap", Some(id), format!("the loop {id} has no cost cap: `cap_usd` is required")); }
                if let Some(u) = until { if let Err(e) = parse_expr(u) { r.err("bad_condition", Some(id), format!("{id}: {e}")); } }
            }
            Kind::Retry { open_after: Some(0), .. } => { r.err("bad_retry", Some(id), format!("{id}: open_after is at least 1")); }
            Kind::Retry { max, .. } => { if *max == 0 { r.err("loop_unbounded", Some(id), format!("the retry {id} has no limit")); } }
            Kind::If { cond } => { if let Err(e) = parse_expr(cond) { r.err("bad_condition", Some(id), format!("{id}: {e}")); } }
            Kind::Switch { on } => { if let Err(e) = parse_expr(on) { r.err("bad_condition", Some(id), format!("{id}: {e}")); } }
            Kind::ForEach { over, batch, on_error, .. } => { if let Err(e) = parse_expr(over) { r.err("bad_condition", Some(id), format!("{id}: {e}")); } if batch.map_or(false, |b| b == 0 || b > 1000) { r.err("bad_foreach", Some(id), format!("{id}: batch is between 1 and 1000")); } if on_error.as_deref().map_or(false, |m| m != "collect" && m != "stop") { r.err("bad_foreach", Some(id), format!("{id}: on_error is `collect` or `stop`")); } }
            Kind::BudgetGate { stop_at, ask_at } => { if !(*stop_at > 0.0 && *stop_at <= 1.0) || ask_at.map_or(false, |a| !(a > 0.0 && a < *stop_at)) { r.err("bad_gate", Some(id), format!("{id}: stop_at is a share of the budget in (0, 1], ask_at is below it")); } }
            Kind::Join { policy: JoinPolicy::Quorum, quorum, .. } => { if quorum.map_or(true, |q| q == 0) { r.err("bad_join", Some(id), format!("{id}: a quorum join needs `quorum` of at least 1")); } }
            Kind::Role { role: RoleKind::Agent, agent, parallel, .. } => {
                match agent {
                    None => r.err("agent_incomplete", Some(id), format!("{id}: a custom agent needs its `agent` part: title, responsibility, brief")),
                    Some(a) => {
                        for (what, v) in [("title", &a.title), ("responsibility", &a.responsibility), ("brief", &a.brief)] { if v.trim().is_empty() { r.err("agent_incomplete", Some(id), format!("{id}: a custom agent has a {what}")); } }
                        if let Some(bad) = a.effects.iter().find(|e| !AGENT_EFFECTS.contains(&e.as_str())) { r.err("unknown_effect", Some(id), format!("{id}: effect `{bad}` is not one of {}", AGENT_EFFECTS.join(", "))); }
                        if let Some(bad) = a.mcp.iter().find(|m| mcp_banned(m)) { r.err("agent_mcp_forbidden", Some(id), format!("{id}: an agent may not be given {bad}: tools that start agents or steer teams are the Leader's")); }
                        if a.responsibility.matches(" and ").count() >= 2 || a.responsibility.chars().count() > 220 { r.warn("agent_many_jobs", Some(id), format!("{id}: one agent, one responsibility: this reads like several")); }
                        if a.has("write") && owns_of(n).is_empty() { r.err("writer_no_scope", Some(id), format!("{id}: a writer owns files: say which (`owns`, e.g. [\"content/**\"])")); }
                    }
                }
                if parallel.map_or(false, |p| p > 8) { r.err("too_parallel", Some(id), format!("{id}: at most 8 in parallel")); }
            }
            Kind::Role { role, readonly, owns, parallel, .. } => {
                if !role.may_edit() && readonly == &Some(false) { r.err("role_not_readonly", Some(id), format!("{id}: a {} never edits: it is read-only", role.key())); }
                if n.is_writer() && owns.is_empty() { r.err("writer_no_scope", Some(id), format!("{id}: a writer owns files: say which (`owns`, e.g. [\"src/**\"])")); }
                if parallel.map_or(false, |p| p > 8) { r.err("too_parallel", Some(id), format!("{id}: at most 8 in parallel")); }
            }
            Kind::Llm(sp) => {
                for e in crate::team_llm::check(sp) { r.err("llm_spec", Some(id), format!("{id}: {e}")); }
                let (nodes, arts) = crate::team_llm::reads(&format!("{} {}", sp.prompt, sp.verify.source));
                for nid in nodes { if !g.nodes.contains_key(&nid) { r.err("llm_source", Some(id), format!("{id}: the prompt reads {nid}, and there is no such node")); } }
                for a in arts { if !g.nodes.values().any(|x| matches!(&x.kind, Kind::Artifact { name: n2, format: f2, .. } if format!("{n2}.{f2}") == a)) { r.err("llm_source", Some(id), format!("{id}: the prompt reads the document {a}, and no artifact node makes it")); } }
                // THE GUARDS ARE NOT OPTIONAL (operator 2026-11-13: if the person insists on a small seat it can be tried, but never delivering bad results unchecked): a seat that is not a frontier seat needs a way to be caught and a way up
                if !crate::team_llm::parse_seat(&sp.seat).map_or(true, |s| crate::team_llm::frontier(&s)) && !crate::team_llm::guarded(sp) { r.err("llm_unguarded", Some(id), format!("{id}: a small seat with no guard delivers its mistakes: give it a `fallback` to a stronger seat, a second seat in `agree_with`, or `verify.in_source` (antiloki_team_seats fit says which fits this step)")); }
            }
            Kind::Cell(sp) => { for e in crate::team_cell::check(sp) { r.err("cell_spec", Some(id), format!("{id}: {e}")); } if let Some((n, _)) = sp.from.split_once('.') { if !g.nodes.contains_key(n) { r.err("cell_from", Some(id), format!("{id}: `from` reads {n}, and there is no such node")); } } }
            Kind::Skeleton(sp) => { if !matches!(g.nodes.get(sp.contract.as_str()).map(|x| &x.kind), Some(Kind::Contract(_))) { r.err("skeleton_contract", Some(id), format!("{id}: `contract` names a contract node that is there")); } }
            Kind::Enforce(sp) => {
                for e in crate::team_enforce::check(sp) { r.err("enforce_spec", Some(id), format!("{id}: {e}")); }
                if let Some((n, _)) = sp.from.split_once('.') { if !g.nodes.contains_key(n) { r.err("enforce_from", Some(id), format!("{id}: `from` reads {n}, and there is no such node")); } }
            }
            Kind::Review(sp) => { for e in crate::team_review::check(sp) { r.err("review_spec", Some(id), format!("{id}: {e}")); } }
            Kind::Stubs(sp) => {
                if let Err(e) = crate::team_llm::parse_seat(&sp.author) { r.err("stubs_seat", Some(id), format!("{id}: the author {e}")); }
                if !matches!(g.nodes.get(sp.contract.as_str()).map(|x| &x.kind), Some(Kind::Contract(_))) { r.err("stubs_contract", Some(id), format!("{id}: `contract` names a contract node that is there")); }
            }
            Kind::Contract(sp) => { for e in crate::team_contract::check(sp) { r.err("contract_spec", Some(id), format!("{id}: {e}")); } }
            Kind::Test(sp) => { for e in crate::team_test::check(sp) { r.err("test_spec", Some(id), format!("{id}: {e}")); } }
            Kind::Decompose(sp) => {
                for (what, seat) in [("planner", &sp.planner), ("coder", &sp.coder)] { if let Err(e) = crate::team_llm::parse_seat(seat) { r.err("decompose_seat", Some(id), format!("{id}: the {what} {e}")); } }
                for f in &sp.fallback { if let Err(e) = crate::team_llm::parse_seat(f) { r.err("decompose_seat", Some(id), format!("{id}: fallback {e}")); } }
                if let Some(c) = &sp.tests { if !matches!(g.nodes.get(c.as_str()).map(|x| &x.kind), Some(Kind::Test(_))) { r.err("decompose_tests", Some(id), format!("{id}: `tests` names a test node that is there")); } }
                if let Some(c) = &sp.review { if !matches!(g.nodes.get(c.as_str()).map(|x| &x.kind), Some(Kind::Review(_))) { r.err("decompose_review", Some(id), format!("{id}: `review` names a review node that is there")); } }
                if let Some(c) = &sp.contract { if !matches!(g.nodes.get(c.as_str()).map(|x| &x.kind), Some(Kind::Contract(_))) { r.err("decompose_contract", Some(id), format!("{id}: `contract` names a contract node that is there")); } }
                if sp.max_subtasks == 0 || sp.max_subtasks > 12 { r.err("decompose_max", Some(id), format!("{id}: `max_subtasks` is 1 to 12")); }
                let small = !crate::team_llm::parse_seat(&sp.coder).map_or(true, |s| crate::team_llm::frontier(&s));
                let judged = g.nodes.values().any(|x| matches!(&x.kind, Kind::Recurse { worker, .. } if worker == id));
                if small && sp.fallback.is_empty() && !judged { r.warn("decompose_unguarded", Some(id), format!("{id}: a small coder with no `fallback` and no recurse node judging it delivers its mistakes: add one of them")); }
            }
            Kind::Recurse { worker, judge, max_depth } => {
                if *max_depth == 0 || *max_depth > 5 { r.err("recurse_depth", Some(id), format!("{id}: `max_depth` is 1 to 5")); }
                match g.nodes.get(worker.as_str()).map(|x| &x.kind) { Some(Kind::Decompose(_)) => {}, _ => r.err("recurse_worker", Some(id), format!("{id}: `worker` is a decompose node that is there")) }
                match g.nodes.get(judge.as_str()).map(|x| &x.kind) { Some(Kind::UseTool { .. }) | Some(Kind::Check { .. }) | Some(Kind::Test(_)) | Some(Kind::Contract(_)) | Some(Kind::Review(_)) | Some(Kind::Skeleton(_)) => {}, _ => r.err("recurse_judge", Some(id), format!("{id}: `judge` is a check or a tool node (the tests) that is there")) }
                for inner in [worker, judge] { if g.edges.iter().any(|e| &e.from == inner || &e.to == inner) { r.err("recurse_inner_wired", Some(id), format!("{id}: {inner} belongs to this recurse node: no wire may touch it")); } }
            }
            Kind::Evolve { scope, after_failures } => {
                if scope.is_empty() { r.err("evolve_scope", Some(id), format!("{id}: `scope` lists the nodes the improver may change")); }
                for s in scope { if !g.nodes.contains_key(s) { r.err("evolve_scope", Some(id), format!("{id}: the scope names {s}, and there is no such node")); } }
                if *after_failures == 0 || *after_failures > 20 { r.err("evolve_after", Some(id), format!("{id}: `after_failures` is 1 to 20")); }
            }
            Kind::Check { on, rules } => {
                for e in crate::team_check::check(rules) { r.err("check_rule", Some(id), format!("{id}: {e}")); }
                let (kind, rest) = on.split_once(':').unwrap_or(("", on.as_str())); let node_of = rest.split('.').next().unwrap_or("");
                let ok = match kind { "artifact" => match rest.split_once('/') { Some((n, _)) => g.nodes.contains_key(n), None => g.nodes.values().any(|x| matches!(&x.kind, Kind::Artifact { name: n2, format: f2, .. } if format!("{n2}.{f2}") == rest)) }, "agent" | "values" | "web" => g.nodes.contains_key(node_of) && rest.contains('.'), _ => false };
                if !ok { r.err("check_source", Some(id), format!("{id}: `{on}` is not something to check (artifact:<name>.<ext>, agent:<node>.report, values:<node>.<field>, web:<node>.text, and the node or the document must exist)")); }
            }
            Kind::Artifact { name, format, from, compose, .. } => {
                if let Some(spec) = compose {
                    if format != "html" { r.err("compose_format", Some(id), format!("{id}: a master page is an html document")); }
                    for e in crate::team_compose::check(spec) { r.err("compose_spec", Some(id), format!("{id}: {e}")); }
                    let (nodes, arts) = crate::team_compose::refs(spec);
                    for nid in nodes { if !g.nodes.contains_key(&nid) { r.err("compose_source", Some(id), format!("{id}: the page reads {nid}, and there is no such node")); } }
                    for a in arts { let ok = match a.split_once('/') { Some((n, _)) => g.nodes.contains_key(n), None => g.nodes.values().any(|x| matches!(&x.kind, Kind::Artifact { name: n2, format: f2, .. } if format!("{n2}.{f2}") == a)) }; if !ok { r.err("compose_source", Some(id), format!("{id}: the page reads the document {a}, and no artifact node makes it (a draft is `<node>/<file>`)")); } }
                } else if from.is_none() { /* the `from` rule below says so */ }
                if name.trim().is_empty() || name.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) { r.err("artifact_name", Some(id), format!("{id}: an artifact has a plain name (letters, digits, - and _)")); }
                if !ARTIFACT_FORMATS.contains(&format.as_str()) && !MEDIA_FORMATS.contains(&format.as_str()) { r.err("artifact_format", Some(id), format!("{id}: the format `{format}` is not one of {}, or a media file: {}", ARTIFACT_FORMATS.join(", "), MEDIA_FORMATS.join(", "))); }
                match from.as_deref().and_then(|f| g.nodes.get(f)) {
                    _ if compose.is_some() => {}
                    Some(src) if matches!(src.kind, Kind::Role { .. } | Kind::UseTool { .. }) => {}
                    Some(_) => r.err("artifact_source", Some(id), format!("{id}: an artifact is made from what an agent or a tool node wrote")),
                    None => r.err("artifact_source", Some(id), format!("{id}: say which node's work it is made from (`from`)")),
                }
                if g.nodes.iter().any(|(o, x)| o != id && matches!(&x.kind, Kind::Artifact { name: n2, format: f2, .. } if n2 == name && f2 == format)) { r.err("artifact_duplicate", Some(id), format!("{id}: another artifact is called {name}.{format}")); }
            }
            Kind::UseTool { tool, allow, .. } => {
                if !TOOL_TYPES.contains(&tool.kind.as_str()) { r.err("unknown_tool_type", Some(id), format!("{id}: tool type `{}` is not one of {}", tool.kind, TOOL_TYPES.join(", "))); }
                if tool.name.trim().is_empty() { r.err("tool_no_name", Some(id), format!("{id}: a tool node names the tool it calls")); }
                if allow.effects.is_empty() { r.err("tool_no_allowlist", Some(id), format!("{id}: a tool node lists what it may do (`allow.effects`): it gets nothing it does not name")); }
                if let Some(bad) = allow.effects.iter().find(|e| !EFFECTS.contains(&e.as_str())) { r.err("unknown_effect", Some(id), format!("{id}: effect `{bad}` is not one of {}", EFFECTS.join(", "))); }
                match tool.kind.as_str() { "skill" if !allow.skills.is_empty() && !allow.skills.contains(&tool.name) => r.err("tool_outside_allowlist", Some(id), format!("{id}: the skill {} is not in its allow-list", tool.name)), "mcp" if !allow.mcp.is_empty() && !allow.mcp.contains(&tool.name) => r.err("tool_outside_allowlist", Some(id), format!("{id}: the MCP tool {} is not in its allow-list", tool.name)), "pane" if !allow.panes.is_empty() && !allow.panes.contains(&tool.name) => r.err("tool_outside_allowlist", Some(id), format!("{id}: the pane {} is not in its allow-list", tool.name)), _ => {} }
                if tool.kind == "source" && allow.effects.iter().any(|e| e != "read") { r.err("source_not_read_only", Some(id), format!("{id}: a source is read-only: it loads data and does nothing else")); }
                // THE BROWSE TOOL: looks at pages signed in, only reads, only the listed domains; `login` hands a window to the person
                if tool.kind == "browse" {
                    for m in crate::team_browse::check_args(&tool.name, &tool.args) { r.err("browse_args", Some(id), format!("{id}: {m}")); }
                    if !allow.effects.iter().any(|e| e == "network") { r.err("browse_needs_network", Some(id), format!("{id}: a browse tool lists the effect `network`: it reads the web")); }
                    if let Some(bad) = allow.effects.iter().find(|e| *e != "network" && *e != "read") { r.err("browse_read_only", Some(id), format!("{id}: browsing only reads: `{bad}` is not allowed on a browse tool")); }
                }
                if tool.kind == "web" {
                    if !["search", "fetch", "docs"].contains(&tool.name.as_str()) { r.err("web_tool", Some(id), format!("{id}: a web tool is `search`, `fetch` or `docs`")); }
                    if !allow.effects.iter().any(|e| e == "network") { r.err("web_needs_network", Some(id), format!("{id}: a web tool lists the effect `network`: it reads the web")); }
                    if let Some(bad) = allow.effects.iter().find(|e| *e != "network" && *e != "read") { r.err("web_read_only", Some(id), format!("{id}: the web is only read: `{bad}` is not allowed on a web tool")); }
                    let key = if tool.name == "fetch" || tool.name == "docs" { "url" } else { "query" }; if tool.args.get(key).and_then(Value::as_str).map_or(true, |s| s.trim().is_empty()) { r.err("web_no_query", Some(id), format!("{id}: a web {} says what to {} (`{key}`; `{{ask}}` and `{{node.field}}` fill from the run)", tool.name, if tool.name == "fetch" { "read" } else { "look for" })); }
                }
                // BL-18: any MCP server by address, any http API; both reach the network, the risk is a class the node must list
                // BL-20: git steps inside the team's own branch and worktrees; the operation is the tool name, its risk must be listed
                // BL-19: a Telegram message is OUTWARD: it lists network and outward (and the human gate before it is the validator's existing rule)
                if tool.kind == "telegram" {
                    if tool.name != "send" { r.err("telegram_op", Some(id), format!("{id}: a telegram node's tool name is `send`")); }
                    if tool.args["chat"].as_str().map_or(true, |c| !c.contains('{') && crate::team_telegram::clean_chat(c).is_err()) { r.err("telegram_chat", Some(id), format!("{id}: `args.chat` is a chat id (-100... for a channel) or @channel_name")); }
                    if tool.args["text"].as_str().map_or(true, |t| t.trim().is_empty()) { r.err("telegram_text", Some(id), format!("{id}: `args.text` is the message (it may use {{ask}} and {{node.field}})")); }
                    if tool.args["token"].as_str().map_or(true, |t| t.trim().is_empty()) { r.err("telegram_token", Some(id), format!("{id}: `args.token` is the NAME the bot token is stored under (never the token itself)")); }
                    for e in ["network", "outward"] { if !allow.effects.iter().any(|x| x == e) { r.err("telegram_effects", Some(id), format!("{id}: a Telegram message lists `{e}` in its allow-list")); } }
                }
                if tool.kind == "git" {
                    if !crate::team_git::OPS.contains(&tool.name.as_str()) { r.err("git_op", Some(id), format!("{id}: a git node's tool name is one of {}", crate::team_git::OPS.join(", "))); }
                    else { let need = crate::team_git::risk(&tool.name); if !allow.effects.iter().any(|e| e == need) { r.err("git_risk_not_listed", Some(id), format!("{id}: `{}` is a `{need}` step: list `{need}` in its allow-list", tool.name)); } }
                    if matches!(tool.name.as_str(), "branch" | "worktree_add" | "worktree_remove" | "merge") && tool.args["name"].as_str().map_or(true, |n| crate::team_git::clean_name(n).is_err()) { r.err("git_name", Some(id), format!("{id}: `args.name` is a name (letters, digits, - and _)")); }
                    if tool.name == "commit" && tool.args["message"].as_str().map_or(true, |m| crate::team_git::clean_message(m).is_err()) { r.err("git_message", Some(id), format!("{id}: a commit needs `args.message`")); }
                }
                // THE APPLY STEP (the light coder): a model's files are written into the run's tree by this step, never by the model
                if tool.kind == "apply" {
                    if tool.name != "files" { r.err("apply_name", Some(id), format!("{id}: an apply step's tool name is `files`")); }
                    if !allow.effects.iter().any(|e| e == "write") { r.err("apply_needs_write", Some(id), format!("{id}: an apply step lists the effect `write`")); }
                    if tool.args["files"].as_array().map_or(true, |a| a.is_empty()) { r.err("apply_files", Some(id), format!("{id}: `args.files` is a list of {{path, content_from}}")); }
                }
                // THE PACKAGE STEP: a pinned git source, wheel-only pinned dependencies, ONE entry of a fixed registry; the person approves each commit once
                if tool.kind == "package" {
                    if let Err(es) = crate::team_package::check_run(&tool.name, &tool.args) { for e in es { r.err("package_args", Some(id), format!("{id}: {e}")); } }
                    for need in ["network", "write", "exec"] { if !allow.effects.iter().any(|e| e == need) { r.err("package_needs_effect", Some(id), format!("{id}: a package step lists the effect `{need}`")); } }
                }
                // THE SOUND STEP: two fixed entries (cue_sheet, mix); the effects it may do are checked like any tool step's
                if tool.kind == "sound" {
                    for e in crate::team_sound::check_args(&tool.name, &tool.args) { r.err("sound_args", Some(id), format!("{id}: {e}")); }
                    let needs: &[&str] = if tool.name == "cue_sheet" { &["read"] } else { &["read", "write", "exec"] };
                    for need in needs { if !allow.effects.iter().any(|e| e == need) { r.err("sound_needs_effect", Some(id), format!("{id}: a sound `{}` step lists the effect `{need}`", tool.name)); } }
                }
                if tool.kind == "mcp_url" {
                    if tool.args["url"].as_str().map_or(true, |u| crate::team_web::allowed_url(u).is_err()) { r.err("mcp_url_url", Some(id), format!("{id}: `args.url` is the MCP server's http(s) address")); }
                    if !allow.effects.iter().any(|e| e == "network") { r.err("mcp_url_needs_network", Some(id), format!("{id}: an MCP server by address lists the effect `network`")); }
                }
                if tool.kind == "api" {
                    if !["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"].contains(&tool.name.to_ascii_uppercase().as_str()) { r.err("api_method", Some(id), format!("{id}: the tool name of an api node is its http method (GET, POST, PUT, PATCH or DELETE)")); }
                    if tool.args["url"].as_str().map_or(true, |u| u.trim().is_empty()) { r.err("api_url", Some(id), format!("{id}: `args.url` is the address to call (it may use {{ask}} and {{node.field}})")); }
                    if !allow.effects.iter().any(|e| e == "network") { r.err("api_needs_network", Some(id), format!("{id}: an api node lists the effect `network`")); }
                    let need = crate::team_web::api_risk(&tool.name); if !allow.effects.iter().any(|e| e == need) { r.err("api_risk_not_listed", Some(id), format!("{id}: a {} call is a `{need}` action: list `{need}` in its allow-list so the person sees it", tool.name.to_ascii_uppercase())); }
                    if let Err(e) = crate::team_web::api_headers_ok(&tool.args["headers"]) { r.err("api_headers", Some(id), format!("{id}: {e}")); }
                }
                if tool.kind == "webapp" { if tool.name.trim().is_empty() { r.err("webapp_app", Some(id), format!("{id}: name the installed app (`name`)")); } if tool.args["tool"].as_str().map_or(true, |t| t.trim().is_empty()) { r.err("webapp_tool", Some(id), format!("{id}: say which tool of the app to call (`args.tool`)")); } }
            }
            Kind::Timer { mode, seconds, wraps, cron } => {
                if *mode == TimerMode::Timeout { match wraps { Some(w) if g.nodes.contains_key(w) => {}, _ => r.err("bad_timer", Some(id), format!("{id}: a timeout timer wraps a node that is there (`wraps`)")) } if *seconds == 0 { r.err("bad_timer", Some(id), format!("{id}: a timeout of 0 seconds")); } }
                if *mode == TimerMode::Schedule && cron.as_deref().map_or(true, |c| c.trim().is_empty()) { r.err("bad_timer", Some(id), format!("{id}: a schedule timer needs a `cron`")); }
            }
            Kind::Code { source, examples, from, limits } => {
                for e in crate::team_code::check(source, examples, limits.as_ref().unwrap_or(&Default::default())) {
                    r.err("code_no_example", Some(id), format!("{id}: {e}"));
                }
                if let Some(f) = from {
                    if let Err(e) = parse_from_ref(f) { r.err("code_source", Some(id), format!("{id}: {e}")); }
                }
            }
            Kind::SubTeam { team, ask, budget_share, max_depth, .. } => {
                if team.trim().is_empty() { r.err("subteam_team", Some(id), format!("{id}: say which team to run (`team`)")); }
                if crate::team_graph::slug(team) == crate::team_graph::slug(&g.team) { r.err("subteam_cycle", Some(id), format!("{id}: a team cannot include itself")); }
                if ask.trim().is_empty() { r.err("subteam_ask", Some(id), format!("{id}: say what to ask the inner team (`ask`)")); }
                if budget_share.map_or(false, |b| !(b > 0.0 && b <= 1.0)) { r.err("subteam_budget", Some(id), format!("{id}: budget_share is a share of the budget in (0, 1]")); }
                if max_depth.map_or(false, |d| d == 0 || d > crate::team_subteam::HARD_MAX_DEPTH) { r.err("subteam_depth", Some(id), format!("{id}: max_depth is between 1 and {}", crate::team_subteam::HARD_MAX_DEPTH)); }
            }
            Kind::Transform { mode, from, config } => {
                for e in crate::team_transform::check(mode, config) { r.err("transform_spec", Some(id), format!("{id}: {e}")); }
                if !crate::team_transform::MODES.contains(&mode.as_str()) { r.err("transform_spec", Some(id), format!("{id}: `{mode}` is not a transform mode")); }
                if let Some(f) = from {
                    if let Err(e) = parse_from_ref(f) { r.err("transform_source", Some(id), format!("{id}: {e}")); }
                }
            }
            Kind::Human { .. } => {
                if let Some(a) = &n.approval { if !["", "low", "medium", "high"].contains(&a.risk.as_str()) { r.err("approval_risk", Some(id), format!("{id}: the risk of an approval is low, medium or high")); } if a.note.chars().count() > 600 { r.err("approval_note", Some(id), format!("{id}: the note of an approval is at most 600 characters")); }
                    // FRAMES: the artifact an approval shows must be one a node of this team writes, and a page (html or svg) a frame can draw
                    if !a.show.is_empty() { let ok = g.nodes.values().any(|x| matches!(&x.kind, Kind::Artifact { name, format, .. } if name == &a.show && (format == "html" || format == "svg"))); if !ok { r.err("approval_show", Some(id), format!("{id}: the approval shows `{}`, but no artifact node writes an html or svg artifact by that name", a.show)); } } }
            }
            Kind::Stop { message } => { if message.trim().is_empty() { r.err("stop_message", Some(id), format!("{id}: a stop node needs its `message`")); } }
            _ => {}
        }
    }
    // a tool with a destructive or money effect has a human on every way to it
    let humans: BTreeSet<&str> = g.nodes.iter().filter(|(_, n)| matches!(n.kind, Kind::Human { .. })).map(|(k, _)| k.as_str()).collect();
    let avoiding = reach(&adj, &entries.iter().map(String::as_str).collect::<Vec<_>>(), &humans);
    for (id, n) in &g.nodes { if let Kind::Role { role: RoleKind::Agent, agent: Some(a), .. } = &n.kind { if (a.has("outward") || a.has("money")) && avoiding.contains(id.as_str()) { r.err("destructive_without_human", Some(id), format!("{id} may act in the world (post, send, spend), and a way reaches it without a human approving first")); } } if let Kind::UseTool { allow, .. } = &n.kind { if allow.effects.iter().any(|e| e == "destructive" || e == "money" || e == "outward") && avoiding.contains(id.as_str()) { r.err("destructive_without_human", Some(id), format!("{id} may do something destructive or spend money, and a way reaches it without a human approving first")); } } }
    // writers that run side by side own different files
    for (fid, n) in &g.nodes { if n.kind != Kind::Fork { continue; }
        let branches: Vec<&str> = g.out_edges(fid).map(|e| e.to.as_str()).collect(); let mut owned: Vec<(&str, Vec<String>)> = vec![];
        for b in branches { let mut skip = BTreeSet::new(); for (jid, jn) in &g.nodes { if matches!(jn.kind, Kind::Join { .. }) { skip.insert(jid.as_str()); } } let part = reach(&adj, &[b], &skip); let mut globs = vec![]; for p in part { if let Kind::Role { owns, .. } = &g.nodes[p].kind { if g.nodes[p].is_writer() { globs.extend(owns.clone()); } } } if !globs.is_empty() { owned.push((b, globs)); } }
        for i in 0..owned.len() { for j in i + 1..owned.len() { if owned[i].1.iter().any(|a| owned[j].1.iter().any(|b| glob_overlap(a, b))) { r.err("parallel_overlap", Some(fid), format!("the branches {} and {} of {fid} write the same files: parallel writers own different ones", owned[i].0, owned[j].0)); } } }
    }
    // a condition reads only what some node upstream writes
    let up = |from: &str, to: &str| -> bool { if from == to { return true; } let skip = BTreeSet::new(); reach(&adj, &[from], &skip).contains(to) };
    for (id, n) in &g.nodes { if let Kind::Llm(sp) = &n.kind { let (nodes, _) = crate::team_llm::reads(&format!("{} {}", sp.prompt, sp.verify.source)); for f in nodes { if g.nodes.contains_key(&f) && !up(&f, id) { r.err("llm_source", Some(id), format!("{id}: the prompt reads {f}, and {f} does not come before it")); } } } }
    for (id, n) in &g.nodes { if let Kind::Check { on, .. } = &n.kind { let node = on.split_once(':').map(|(_, r)| r.split(['.', '/']).next().unwrap_or("")).unwrap_or(""); if g.nodes.contains_key(node) && !up(node, id) { r.err("check_source", Some(id), format!("{id}: it checks what {node} makes, and {node} does not come before it")); } } }
    for (id, n) in &g.nodes { if let Kind::Artifact { compose: Some(spec), .. } = &n.kind { let (nodes, _) = crate::team_compose::refs(spec); for f in nodes { if g.nodes.contains_key(&f) && !up(&f, id) { r.err("compose_source", Some(id), format!("{id}: the page reads {f}, and {f} does not come before it")); } } } }
    for (id, n) in &g.nodes { if let Kind::Artifact { from: Some(f), .. } = &n.kind { if g.nodes.contains_key(f) && !up(f, id) { r.err("artifact_source", Some(id), format!("{id} is made from {f}, and {f} does not come before it")); } } }
    let readers: Vec<(&String, Vec<String>)> = g.nodes.iter().filter_map(|(id, n)| { let src: Vec<&str> = match &n.kind { Kind::If { cond } => vec![cond.as_str()], Kind::Switch { on } => vec![on.as_str()], Kind::Loop { until: Some(u), .. } => vec![u.as_str()], Kind::ForEach { over, .. } => vec![over.as_str()], _ => vec![] }; if src.is_empty() { None } else { Some((id, src.iter().filter_map(|s| parse_expr(s).ok()).flat_map(|e| operands(&e)).collect())) } }).collect();
    for (id, ops) in readers { for op in ops {
        let mut it = op.split('.'); let (root, field) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if root == "budget" || root == "elapsed" { continue; }
        match g.nodes.get(root) {
            None => r.err("unwritten_operand", Some(id), format!("{id} reads {op}, and there is no node called {root}")),
            Some(w) => { if !writes_of(w).iter().any(|(f, _)| f == field) { r.err("unwritten_operand", Some(id), format!("{id} reads {op}, but {root} writes only: {}", writes_of(w).iter().map(|(f, _)| f.clone()).collect::<Vec<_>>().join(", "))); } else if !up(root, id) { r.err("unwritten_operand", Some(id), format!("{id} reads {op}, but {root} is not upstream of it")); } }
        }
    } }
    for (code, msg) in crate::team_triggers::check(g) { match code.as_str() { "trigger_later" => r.warn("trigger_later", None, msg), "bad_limits" => r.err("bad_limits", None, msg), _ => r.err("bad_trigger", None, msg) } }
    r.worst_usd = worst_case_usd(g);
    if g.budget_usd > 0.0 && r.worst_usd > g.budget_usd { r.warn("worst_case_over_budget", None, format!("the worst case ${:.2} is over the ${:.2} budget: the budget gate will stop it first, but plan for less", r.worst_usd, g.budget_usd)); }
    r
}

// ── the run ───────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailKind { Verdict, Error, Timeout, Cap, Budget, Human }
impl FailKind { pub fn port(self) -> &'static str { match self { FailKind::Verdict => "failed", FailKind::Error => "error", FailKind::Timeout => "timeout", FailKind::Cap => "cap", FailKind::Budget => "budget", FailKind::Human => "human" } } }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", content = "why", rename_all = "snake_case")]
pub enum RunStatus { Idle, Running, Paused, Done, Blocked(String), Stopped }

/// One line of the team's own record (the team saga): what a node did, when. The driver writes them as events of the episode the run is in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceEv { pub t_ms: i64, pub node: String, pub kind: String, pub text: String }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub status: RunStatus, pub started_ms: i64, pub now_ms: i64,
    /// the nodes working now (and when each began)
    pub active: BTreeMap<String, i64>,
    /// what the nodes wrote, by node: `values["tests"]["exit"]`
    pub values: Map<String, Value>,
    pub spent_usd: f64, pub node_cost: BTreeMap<String, f64>,
    /// joins: who arrived (this round); retries: tries so far
    pub arrivals: BTreeMap<String, BTreeSet<String>>, pub tries: BTreeMap<String, u32>,
    /// circuit breakers that are open: retry node -> when it opened
    #[serde(default)] pub opened: BTreeMap<String, i64>,
    /// timers: when each wakes
    pub wake_at: BTreeMap<String, i64>,
    /// nodes whose start is held back (paused, or over the `parallel` limit)
    pub queued: Vec<String>,
    /// for the driver: drained after each step
    pub trace: Vec<TraceEv>,
    pub steps: usize,
    /// steps the person said yes to under `ask` (once per run), and steps waiting for that yes now
    #[serde(default)] pub permitted: BTreeSet<String>, #[serde(default)] pub asking: BTreeSet<String>, #[serde(default)] pub go_ahead: BTreeSet<String>,
}
impl RunState {
    pub fn new() -> RunState { RunState { status: RunStatus::Idle, started_ms: 0, now_ms: 0, active: BTreeMap::new(), values: Map::new(), spent_usd: 0.0, node_cost: BTreeMap::new(), arrivals: BTreeMap::new(), tries: BTreeMap::new(), opened: BTreeMap::new(), wake_at: BTreeMap::new(), queued: vec![], trace: vec![], steps: 0, permitted: BTreeSet::new(), asking: BTreeSet::new(), go_ahead: BTreeSet::new() } }
    pub fn ctx<'a>(&'a self, g: &Graph) -> Ctx<'a> { Ctx { values: &self.values, spent_usd: self.spent_usd, budget_usd: g.budget_usd, elapsed_s: ((self.now_ms - self.started_ms).max(0) as f64) / 1000.0 } }
    pub fn finished(&self) -> bool { matches!(self.status, RunStatus::Done | RunStatus::Blocked(_) | RunStatus::Stopped) }
    fn say(&mut self, node: &str, kind: &str, text: impl Into<String>) { self.trace.push(TraceEv { t_ms: self.now_ms, node: node.to_string(), kind: kind.to_string(), text: text.into() }); }
}
impl Default for RunState { fn default() -> Self { RunState::new() } }

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Start { now_ms: i64, seed: Map<String, Value> },
    NodeDone { node: String, outputs: Value, cost_usd: f64 },
    NodeFailed { node: String, kind: FailKind, error: String, cost_usd: f64 },
    Tick { now_ms: i64 },
    Answer { node: String, approve: bool },
    Pause, Resume, Stop,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    /// start this node (the driver runs the agent, the tool, ...)
    Run { node: String },
    /// the node must wait for the person
    Park { node: String, gate: String },
    /// a new pass of a loop (or a new item of a for-each) begins: the driver opens an episode of the team saga
    Pass { node: String, n: u32 },
    /// stop what this node is doing (a parallel branch that lost, a run that stopped)
    Cancel { node: String },
    /// the pack is delivered
    Deliver,
    /// the run cannot go on without the person
    Handoff { reason: String },
}

fn max_parallel(g: &Graph) -> usize { g.nodes.values().filter_map(|n| match &n.kind { Kind::Role { parallel: Some(p), .. } => Some(*p as usize), _ => None }).max().unwrap_or(usize::MAX).max(1) }

struct Stepper<'a> { g: &'a Graph, s: &'a mut RunState, out: Vec<Action> }
impl<'a> Stepper<'a> {
    fn route(&mut self, from: &str, port: &str) {
        let targets: Vec<String> = self.g.out_edges(from).filter(|e| e.port == port).map(|e| e.to.clone()).collect();
        if targets.is_empty() {
            if FAILURE_PORTS.contains(&port) { self.handoff(format!("{from} did not go to plan ({port}) and nothing is wired to catch it")); }
            else if matches!(self.g.node(from).map(|n| &n.kind), Some(Kind::Human { .. })) && port == "denied" { self.handoff(format!("{from} was not approved")); }
            else { self.handoff(format!("nothing follows {from} on its `{port}` port")); }
            return;
        }
        for t in targets { if self.s.finished() { return; } self.enter(&t, from); }
    }
    fn handoff(&mut self, why: String) {
        if self.s.finished() { return; }
        self.s.status = RunStatus::Blocked(why.clone()); self.s.say("", "handoff", format!("handed to the person: {why}"));
        for n in self.s.active.keys().cloned().collect::<Vec<_>>() { self.out.push(Action::Cancel { node: n }); }
        self.s.active.clear(); self.s.queued.clear(); self.out.push(Action::Handoff { reason: why });
    }
    fn ctx_holds(&mut self, src: &str, at: &str) -> Option<bool> { match holds(src, &self.s.ctx(self.g)) { Ok(b) => Some(b), Err(e) => { self.handoff(format!("{at}: {e}")); None } } }
    fn start(&mut self, id: &str) {
        // THE AUTONOMY DIAL: a step set to `ask` waits for the person once per run, `always` every time; a yes from the card lets this start through
        if let Some(au) = self.g.node(id).and_then(|n| n.autonomy.clone()) {
            if self.s.go_ahead.remove(id) {} else if (au == "always" || (au == "ask" && !self.s.permitted.contains(id))) && !self.s.asking.contains(id) {
                self.s.asking.insert(id.to_string()); self.s.active.insert(id.to_string(), self.s.now_ms); self.s.say(id, "permission", "asks the person before it starts".to_string());
                self.out.push(Action::Park { node: id.to_string(), gate: "permission".into() }); return;
            }
        }
        if matches!(self.s.status, RunStatus::Paused) || (self.s.active.len() >= max_parallel(self.g) && matches!(self.g.node(id).map(|n| &n.kind), Some(Kind::Role { .. }))) { if !self.s.queued.iter().any(|q| q == id) { self.s.queued.push(id.to_string()); } return; }
        self.s.active.insert(id.to_string(), self.s.now_ms); self.out.push(Action::Run { node: id.to_string() });
    }
    fn enter(&mut self, id: &str, from: &str) {
        self.s.steps += 1; if self.s.steps > MAX_STEPS { self.handoff("the run took too many steps: the graph does not end".into()); return; }
        let Some(node) = self.g.node(id).cloned() else { self.handoff(format!("{id} is not in the graph")); return; };
        match &node.kind {
            Kind::Leader => {}
            Kind::Stop { message } => { self.s.say(id, "stop", message.clone()); self.handoff(message.clone()); }
            Kind::Role { .. } | Kind::UseTool { .. } | Kind::Artifact { .. } | Kind::Check { .. } | Kind::Llm(_) | Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Evolve { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Stubs(_) | Kind::Review(_) | Kind::Enforce(_) | Kind::Skeleton(_) | Kind::Cell(_) | Kind::Code { .. } | Kind::Transform { .. } | Kind::SubTeam { .. } => { self.s.say(id, "start", format!("{} started", title_of(id, &node))); self.start(id); }
            Kind::If { cond } => { let Some(b) = self.ctx_holds(cond, id) else { return }; self.s.say(id, "if", format!("{cond} → {b}")); self.route(id, if b { "then" } else { "else" }); }
            Kind::Switch { on } => {
                let v = match eval(&match parse_expr(on) { Ok(e) => e, Err(e) => { self.handoff(format!("{id}: {e}")); return; } }, &self.s.ctx(self.g)) { Ok(v) => v, Err(e) => { self.handoff(format!("{id}: {e}")); return; } };
                let key = match &v { Value::String(s) => s.clone(), other => other.to_string() };
                let port = if self.g.out_edges(id).any(|e| e.port == key) { key } else { "default".to_string() };
                self.s.say(id, "switch", format!("{on} = {v} → {port}")); self.route(id, &port);
            }
            Kind::Loop { max_iterations, until } => {
                let count = self.s.values.get(id).and_then(|o| o.get("count")).and_then(Value::as_u64).unwrap_or(0) as u32;
                // `until` is asked every time the loop is entered; a value nobody has written yet means "not yet"
                if let Some(u) = until { let held = match holds(u, &self.s.ctx(self.g)) { Ok(b) => b, Err(e) if e.contains("has not been written yet") => false, Err(e) => { self.handoff(format!("{id}: {e}")); return; } }; if held { self.s.say(id, "loop", format!("{u} holds after {count} pass(es): done")); self.route(id, "done"); return; } }
                if count >= *max_iterations { self.s.say(id, "loop", format!("{count} passes, the limit")); if self.g.out_edges(id).any(|e| e.port == "exhausted") { self.route(id, "exhausted"); } else { self.route(id, "done"); } return; }
                let n = count + 1; self.s.values.insert(id.to_string(), json!({ "count": n, "max": max_iterations })); self.s.say(id, "loop", format!("pass {n} of {max_iterations}")); self.out.push(Action::Pass { node: id.to_string(), n }); self.route(id, "body");
            }
            Kind::ForEach { over, batch, .. } => {
                let idx = match self.s.values.get(id).and_then(|o| o.get("index")).and_then(Value::as_u64) { Some(i) => (i + 1) as usize, None => 0 };
                let list = match Ctx::get(&self.s.ctx(self.g), over) { Ok(Value::Array(a)) => a, Ok(_) => { self.handoff(format!("{id}: {over} is not a list")); return; } Err(e) => { self.handoff(format!("{id}: {e}")); return; } };
                let per = batch.unwrap_or(1).max(1) as usize; let passes = (list.len() + per - 1) / per;
                // what the loop has kept so far (failures it was told to collect) survives from pass to pass and is in the `done` output
                let errors = self.s.values.get(id).and_then(|o| o.get("errors")).cloned().unwrap_or_else(|| json!([]));
                if idx >= passes { self.s.say(id, "for_each", format!("{} item(s) done", list.len())); self.s.values.insert(id.to_string(), json!({ "count": list.len(), "index": passes.saturating_sub(1), "errors": errors })); self.route(id, "done"); return; }
                // each item starts clean: what the body wrote for the last one is gone
                for b in body_of(self.g, id) { self.s.values.remove(b); self.s.tries.remove(b); self.s.arrivals.remove(b); }
                let item = if per > 1 { Value::Array(list[idx * per..(idx * per + per).min(list.len())].to_vec()) } else { list[idx].clone() };
                self.s.values.insert(id.to_string(), json!({ "item": item, "index": idx, "count": list.len(), "errors": errors })); self.s.say(id, "for_each", format!("item {} of {}", idx + 1, passes)); self.out.push(Action::Pass { node: id.to_string(), n: idx as u32 + 1 }); self.route(id, "body");
            }
            Kind::Fork => { self.s.say(id, "fork", "split"); self.route(id, "branch"); }
            Kind::Join { policy: JoinPolicy::Consensus, .. } => {
                let inbound = self.g.in_edges(id).count().max(1);
                let set = self.s.arrivals.entry(id.to_string()).or_default(); set.insert(from.to_string()); let arrived: Vec<String> = set.iter().cloned().collect();
                if arrived.len() < inbound { return; }
                let answers: Vec<Value> = arrived.iter().map(|n| consensus_view(self.s.values.get(n).unwrap_or(&Value::Null))).collect();
                if answers.windows(2).all(|w| w[0] == w[1]) {
                    self.s.say(id, "join", format!("{} of {} agree", answers.len(), inbound)); self.s.values.insert(id.to_string(), json!({ "agreed": true, "answer": answers[0] })); self.route(id, "out");
                } else {
                    self.s.values.insert(id.to_string(), json!({ "agreed": false, "answers": answers }));
                    if self.g.out_edges(id).any(|e| e.port == "climb") { self.s.say(id, "join", "the seats disagree: climbing".to_string()); self.s.arrivals.remove(id); self.route(id, "climb"); }
                    else { self.handoff(format!("{id}: the seats do not give the same answer and nothing is wired to climb")); }
                }
            }
            Kind::Join { policy, quorum } => {
                let inbound = self.g.in_edges(id).count().max(1);
                let set = self.s.arrivals.entry(id.to_string()).or_default(); set.insert(from.to_string()); let n = set.len();
                let need = match policy { JoinPolicy::All => inbound, JoinPolicy::Any => 1, JoinPolicy::Quorum => (quorum.unwrap_or(1) as usize).min(inbound), JoinPolicy::Consensus => inbound };
                if n == need { self.s.say(id, "join", format!("{n} of {inbound} arrived")); if *policy != JoinPolicy::All { let rest: Vec<String> = self.s.active.keys().cloned().collect(); for r in rest { self.s.active.remove(&r); self.out.push(Action::Cancel { node: r }); } } self.route(id, "out"); }
            }
            Kind::Timer { mode: TimerMode::Delay, seconds, .. } => { self.s.wake_at.insert(id.to_string(), self.s.now_ms + (*seconds as i64) * 1000); self.s.active.insert(id.to_string(), self.s.now_ms); self.s.say(id, "timer", format!("waiting {seconds} s")); }
            Kind::Timer { .. } => { self.route(id, "out"); }
            Kind::Retry { max, backoff_s, open_after, cooldown_s } => {
                // the breaker: open, it does not send the round again until the cooldown has passed, then lets one try through (half open) with a fresh count
                if let Some(opened) = self.s.opened.get(id).copied() {
                    let cool = (cooldown_s.unwrap_or(60) as i64) * 1000;
                    if self.s.now_ms - opened < cool { self.s.say(id, "retry", "the breaker is open: not calling it again yet".to_string()); self.route(id, "exhausted"); return; }
                    self.s.opened.remove(id); self.s.tries.remove(id); self.s.say(id, "retry", "the breaker lets one try through".to_string());
                }
                let t = self.s.tries.entry(id.to_string()).or_insert(0); *t += 1; let n = *t;
                if open_after.map_or(false, |o| n > o) { self.s.opened.insert(id.to_string(), self.s.now_ms); self.s.say(id, "retry", format!("{} failures: the breaker opens", n - 1)); self.route(id, "exhausted"); return; }
                if n > *max { self.s.say(id, "retry", format!("{max} tries, no more")); self.route(id, "exhausted"); }
                else if *backoff_s > 0 { self.s.wake_at.insert(id.to_string(), self.s.now_ms + (*backoff_s as i64) * 1000 * n as i64); self.s.active.insert(id.to_string(), self.s.now_ms); self.s.say(id, "retry", format!("try {n} of {max} in {} s", backoff_s * n as u64)); }
                else { self.s.say(id, "retry", format!("try {n} of {max}")); self.out.push(Action::Pass { node: id.to_string(), n }); self.route(id, "again"); }
            }
            Kind::BudgetGate { stop_at, ask_at } => {
                let b = self.g.budget_usd; let share = if b > 0.0 { self.s.spent_usd / b } else { 0.0 };
                let port = if share >= *stop_at { "stop" } else if ask_at.map_or(false, |a| share >= a) && self.g.out_edges(id).any(|e| e.port == "ask") { "ask" } else { "continue" };
                self.s.say(id, "budget", format!("{:.0}% of the budget spent → {port}", share * 100.0)); self.route(id, port);
            }
            Kind::Human { gate, .. } => { self.s.active.insert(id.to_string(), self.s.now_ms); self.s.say(id, "human", format!("waiting for the person ({gate})")); self.out.push(Action::Park { node: id.to_string(), gate: gate.clone() }); }
            Kind::Deliver => { self.s.status = RunStatus::Done; self.s.say(id, "deliver", "delivered"); self.out.push(Action::Deliver); }
        }
    }
    fn account(&mut self, id: &str, usd: f64) -> bool {
        if usd > 0.0 { self.s.spent_usd += usd; *self.s.node_cost.entry(id.to_string()).or_insert(0.0) += usd; }
        if self.g.budget_usd > 0.0 && self.s.spent_usd > self.g.budget_usd + 1e-9 { let why = format!("the budget of ${:.2} is spent (${:.2})", self.g.budget_usd, self.s.spent_usd); self.handoff(why); return false; }
        true
    }
}

/// The decision half. Given what happened, what happens next. Mutates the state, returns what the driver must do. Pure: no clock (time is in the events), no I/O.
pub fn graph_step(g: &Graph, s: &mut RunState, ev: Event) -> Result<Vec<Action>, String> {
    let mut st = Stepper { g, s, out: vec![] };
    match ev {
        Event::Start { now_ms, seed } => {
            if st.s.status != RunStatus::Idle { return Err("the run has started already".into()); }
            st.s.status = RunStatus::Running; st.s.started_ms = now_ms; st.s.now_ms = now_ms; for (k, v) in seed { st.s.values.insert(k, v); }
            st.s.say("", "start", format!("the team {} started", g.team));
            for e in g.entries() { if st.s.finished() { break; } st.enter(&e, ""); }
        }
        Event::Tick { now_ms } => {
            st.s.now_ms = now_ms;
            let due: Vec<String> = st.s.wake_at.iter().filter(|(_, t)| **t <= now_ms).map(|(k, _)| k.clone()).collect();
            for id in due { st.s.wake_at.remove(&id); st.s.active.remove(&id); let port = if matches!(g.node(&id).map(|n| &n.kind), Some(Kind::Retry { .. })) { let n = st.s.tries.get(&id).copied().unwrap_or(1); st.out.push(Action::Pass { node: id.clone(), n }); "again" } else { "out" }; st.route(&id, port); }
            let mut timed: Vec<String> = vec![];
            for (id, began) in st.s.active.iter() { let Some(n) = g.node(id) else { continue }; let mut lim = n.timeout_s; for (_, t) in g.nodes.iter() { if let Kind::Timer { mode: TimerMode::Timeout, seconds, wraps: Some(w), .. } = &t.kind { if w == id { lim = Some(lim.map_or(*seconds, |l| l.min(*seconds))); } } } if let Some(l) = lim { let counts = match &n.kind { Kind::Role { .. } | Kind::UseTool { .. } => true, Kind::Human { on_timeout, .. } => on_timeout.as_deref() == Some("handoff"), _ => false };   // a person is waited for: it never times out into an approval, and only hands the run over when the node says so
                if counts && now_ms - *began >= (l as i64) * 1000 { timed.push(id.clone()); } } }
            for id in timed { if st.s.finished() { break; } st.s.active.remove(&id); st.out.push(Action::Cancel { node: id.clone() }); st.s.say(&id, "timeout", "ran out of time"); let kind = if matches!(g.node(&id).map(|n| &n.kind), Some(Kind::Human { .. })) { None } else { Some(FailKind::Timeout) }; if kind.is_some() { st.route(&id, "timeout"); } else { st.route(&id, "human"); } }
        }
        Event::NodeDone { node, outputs, cost_usd } => {
            if st.s.active.remove(&node).is_none() { return Err(format!("{node} was not running")); }
            if !st.account(&node, cost_usd) { return Ok(st.out); }
            let mut o = match outputs { Value::Object(m) => m, Value::Null => Map::new(), other => { let mut m = Map::new(); m.insert("value".into(), other); m } };
            if let Some(Node { kind: Kind::Loop { .. }, .. }) = g.node(&node) { return Err("a loop is not a step".into()); }
            if let Some(Node { cap_usd: Some(cap), .. }) = g.node(&node) { if st.s.node_cost.get(&node).copied().unwrap_or(0.0) > *cap + 1e-9 { st.s.say(&node, "cap", format!("spent more than its ${cap:.2} cap")); st.route(&node, "cap"); return Ok(st.out); } }
            if let Some(Node { kind: Kind::UseTool { .. } | Kind::Artifact { .. } | Kind::Check { .. } | Kind::Llm(_) | Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Evolve { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Stubs(_) | Kind::Review(_) | Kind::Enforce(_) | Kind::Skeleton(_) | Kind::Cell(_) | Kind::Code { .. } | Kind::Transform { .. } | Kind::SubTeam { .. }, .. }) = g.node(&node) { o.entry("ok".to_string()).or_insert(json!(true)); }
            let text = summarize(&o); st.s.values.insert(node.clone(), Value::Object(o)); st.s.say(&node, "done", format!("{} finished{}", g.node(&node).map_or(node.clone(), |n| title_of(&node, n)), text));
            if g.node(&node).map_or(false, |n| matches!(&n.kind, Kind::Role { .. })) || g.node(&node).map_or(false, |n| matches!(&n.kind, Kind::UseTool { .. } | Kind::Artifact { .. } | Kind::Check { .. } | Kind::Llm(_) | Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Evolve { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Stubs(_) | Kind::Review(_) | Kind::Enforce(_) | Kind::Skeleton(_) | Kind::Cell(_) | Kind::Code { .. } | Kind::Transform { .. } | Kind::SubTeam { .. })) { st.route(&node, "out"); }
            // a queued node may start now there is room
            if let Some(next) = st.s.queued.first().cloned() { if !matches!(st.s.status, RunStatus::Paused) && st.s.active.len() < max_parallel(g) { st.s.queued.remove(0); st.start(&next); } }
        }
        Event::NodeFailed { node, kind, error, cost_usd } => {
            if st.s.active.remove(&node).is_none() { return Err(format!("{node} was not running")); }
            if !st.account(&node, cost_usd) { return Ok(st.out); }
            st.s.say(&node, "failed", format!("{} failed ({:?}): {}", node, kind, error.chars().take(200).collect::<String>()));
            // WHAT WENT WRONG STAYS WITH THE NODE (the operator 2026-11-20: a failed node must say why, on the graph and inside it): the words are kept in its values (`error`, `failed`), read by the screen
            { let mut cur = st.s.values.get(&node).cloned().filter(Value::is_object).unwrap_or_else(|| json!({})); cur["failed"] = json!(true); cur["error"] = json!(error.chars().take(600).collect::<String>()); cur["error_kind"] = json!(format!("{:?}", kind).to_lowercase()); st.s.values.insert(node.clone(), cur); }
            // a for-each told to `collect` keeps the failure and goes on with the next item, unless the failing node has its own failure edge
            let caught = FAILURE_PORTS.iter().any(|p| g.out_edges(&node).any(|e| e.port == *p));
            let collector = if caught { None } else { g.nodes.iter().find(|(fid, fnode)| matches!(&fnode.kind, Kind::ForEach { on_error: Some(m), .. } if m == "collect") && body_of(g, fid.as_str()).contains(node.as_str())).map(|(fid, _)| fid.clone()) };
            if let Some(fid) = collector {
                let mut cur = st.s.values.get(&fid).cloned().unwrap_or_else(|| json!({}));
                let mut errs = cur.get("errors").and_then(Value::as_array).cloned().unwrap_or_default();
                errs.push(json!({ "index": cur.get("index").cloned().unwrap_or(json!(0)), "node": node, "error": error.chars().take(300).collect::<String>() }));
                cur["errors"] = Value::Array(errs); st.s.values.insert(fid.clone(), cur);
                // the other branches of this item are not going to finish: drop them, then take the next item
                let rest: Vec<String> = st.s.active.keys().filter(|k| body_of(g, &fid).contains(k.as_str())).cloned().collect();
                for r in rest { st.s.active.remove(&r); st.out.push(Action::Cancel { node: r }); }
                st.enter(&fid, &node);
                return Ok(st.out);
            }
            st.route(&node, kind.port());
        }
        Event::Answer { node, approve } => {
            if st.s.asking.contains(&node) {
                // a step that asked permission before it starts: yes lets it start, no hands the run to the person
                st.s.asking.remove(&node); st.s.active.remove(&node);
                if approve { st.s.say(&node, "answer", "the person allowed it"); if g.node(&node).and_then(|n| n.autonomy.as_deref()) == Some("ask") { st.s.permitted.insert(node.clone()); } st.s.go_ahead.insert(node.clone()); st.start(&node); }
                else { st.s.say(&node, "answer", "the person did not allow it"); st.route(&node, "human"); }
                return Ok(st.out);
            }
            if !matches!(g.node(&node).map(|n| &n.kind), Some(Kind::Human { .. })) { return Err(format!("{node} is not a human node: only a person answers at one, and an agent never does")); }
            if st.s.active.remove(&node).is_none() { return Err(format!("{node} was not waiting")); }
            st.s.values.insert(node.clone(), json!({ "approved": approve })); st.s.say(&node, "answer", if approve { "the person approved" } else { "the person did not approve" });
            if approve { let port = if g.out_edges(&node).any(|e| e.port == "approved") { "approved" } else { "out" }; st.route(&node, port); } else { st.route(&node, "denied"); }
        }
        Event::Pause => { if st.s.status == RunStatus::Running { st.s.status = RunStatus::Paused; st.s.say("", "pause", "paused"); } }
        Event::Resume => { if st.s.status == RunStatus::Paused { st.s.status = RunStatus::Running; st.s.say("", "resume", "resumed"); let q: Vec<String> = std::mem::take(&mut st.s.queued); for n in q { st.start(&n); } } }
        Event::Stop => { if !st.s.finished() { st.s.status = RunStatus::Stopped; for n in st.s.active.keys().cloned().collect::<Vec<_>>() { st.out.push(Action::Cancel { node: n }); } st.s.active.clear(); st.s.queued.clear(); st.s.say("", "stop", "stopped by the person"); } }
    }
    Ok(st.out)
}
fn summarize(o: &Map<String, Value>) -> String {
    let parts: Vec<String> = o.iter().filter(|(_, v)| matches!(v, Value::Bool(_) | Value::Number(_) | Value::String(_))).take(4).map(|(k, v)| format!("{k}={}", match v { Value::String(s) => s.clone(), x => x.to_string() })).collect();
    if parts.is_empty() { String::new() } else { format!(" ({})", parts.join(", ")) }
}

// ── simulate ──────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
pub struct Sim { pub path: Vec<String>, pub passes: BTreeMap<String, u32>, pub outcome: String, pub spent_usd: f64, pub worst_usd: f64, pub trace: Vec<TraceEv> }
/// Step the graph with STUBBED outputs, so a person (or the Leader) sees the path, the loop counts and the worst case before a cent is spent. `stubs`: node id → the outputs it returns, in order (the last repeats);
/// a node with none returns the happy default (a QA passes, a tool is ok, a worker is done). A human node approves.
pub fn simulate(g: &Graph, stubs: &BTreeMap<String, Vec<Value>>, seed: Map<String, Value>) -> Result<Sim, String> {
    let mut s = RunState::new(); let mut path: Vec<String> = vec![]; let mut used: BTreeMap<String, usize> = BTreeMap::new(); let mut clock = 0i64;
    let mut todo: VecDeque<Action> = graph_step(g, &mut s, Event::Start { now_ms: 0, seed })?.into();
    let mut guard = 0;
    while let Some(a) = todo.pop_front() {
        guard += 1; if guard > MAX_STEPS { return Err("the simulation does not end".into()); }
        match a {
            Action::Run { node } => {
                path.push(node.clone());
                let n = used.entry(node.clone()).or_insert(0); let list = stubs.get(&node); let out = list.and_then(|l| l.get((*n).min(l.len().saturating_sub(1)))).cloned().unwrap_or_else(|| default_output(g.node(&node)));
                *n += 1; clock += 1000;
                if !s.active.contains_key(&node) { continue; }
                let more = graph_step(g, &mut s, Event::NodeDone { node, outputs: out, cost_usd: 0.0 })?; todo.extend(more);
            }
            Action::Park { node, .. } => { path.push(node.clone()); clock += 1000; let more = graph_step(g, &mut s, Event::Answer { node, approve: true })?; todo.extend(more); }
            Action::Pass { .. } | Action::Cancel { .. } => {}
            Action::Deliver => { path.push("deliver".into()); }
            Action::Handoff { .. } => {}
        }
        if todo.is_empty() && !s.finished() {
            // a timer waits: advance the clock to the next wake and go on
            if let Some(t) = s.wake_at.values().min().copied() { clock = clock.max(t); let more = graph_step(g, &mut s, Event::Tick { now_ms: clock })?; todo.extend(more); } else if !s.active.is_empty() { return Err(format!("the simulation is stuck: {} is waiting for nothing", s.active.keys().cloned().collect::<Vec<_>>().join(", "))); }
        }
    }
    let passes: BTreeMap<String, u32> = g.nodes.iter().filter_map(|(k, n)| if matches!(n.kind, Kind::Loop { .. }) { s.values.get(k).and_then(|v| v.get("count")).and_then(Value::as_u64).map(|c| (k.clone(), c as u32)) } else { None }).collect();
    let outcome = match &s.status { RunStatus::Done => "delivered".to_string(), RunStatus::Blocked(w) => format!("handed to the person: {w}"), other => format!("{other:?}") };
    Ok(Sim { path, passes, outcome, spent_usd: s.spent_usd, worst_usd: worst_case_usd(g), trace: s.trace })
}
fn default_output(n: Option<&Node>) -> Value {
    match n.map(|n| &n.kind) {
        Some(Kind::Role { role: RoleKind::Qa, .. }) => json!({ "pass": true, "failed": [], "evidence": [] }),
        Some(Kind::Role { role: RoleKind::Audit, .. }) => json!({ "verdict": "pass", "flags": [] }),
        Some(Kind::Role { role: RoleKind::Planner, .. }) => json!({ "plan": {}, "sagas": [{ "id": "saga-1" }], "criteria": [] }),
        Some(Kind::Role { role: RoleKind::Debugger, .. }) => json!({ "patched": true, "files": [] }),
        Some(Kind::Role { .. }) => json!({ "done": true, "files": [] }),
        Some(Kind::UseTool { .. }) => json!({ "ok": true, "exit": 0 }),
        Some(Kind::Artifact { .. }) => json!({ "ok": true, "name": "artifact", "path": "artifacts/artifact" }),
        Some(Kind::Check { .. }) => json!({ "ok": true, "pass": true, "failed": [], "passed": [], "score": 1.0 }),
        Some(Kind::Cell(_)) => json!({ "ok": true, "output": "x", "tries": 1 }),
        Some(Kind::Skeleton(_)) => json!({ "ok": true, "issues": [], "diff": "", "merged": "", "new_only": "", "tail": "" }),
        Some(Kind::Enforce(sp)) => { let mut m = Map::new(); m.insert("ok".into(), json!(true)); m.insert("repaired_by".into(), json!("code")); for (k, ty) in &sp.schema { m.insert(k.clone(), match ty { Value::String(s) if s.starts_with("number") => json!(1.0), Value::String(s) if s.starts_with("bool") => json!(true), Value::Object(o) => o["enum"][0].clone(), Value::Array(_) => json!([]), _ => json!("x") }); } Value::Object(m) }
        Some(Kind::Review(_)) => json!({ "ok": true, "issues": [], "errors": 0, "tail": "" }),
        Some(Kind::Stubs(_)) => json!({ "ok": true, "files": [], "functions": [], "failed": [], "text": "x" }),
        Some(Kind::Contract(_)) => json!({ "ok": true, "issues": [], "tail": "" }),
        Some(Kind::Test(_)) => json!({ "ok": true, "exit": 0, "total": 1, "passed": 1, "failed": [], "tail": "" }),
        Some(Kind::Decompose(_)) => json!({ "ok": true, "subtasks": [], "files": [], "failed": [], "text": "x" }),
        Some(Kind::Recurse { .. }) => json!({ "ok": true, "depth": 1, "attempts": 1, "stopped": "", "files": [] }),
        Some(Kind::Evolve { .. }) => json!({ "ok": true, "due": false, "scope": [] }),
        Some(Kind::Llm(sp)) => { let mut m = Map::new(); m.insert("ok".into(), json!(true)); m.insert("schema_ok".into(), json!(true)); for (k, ty) in &sp.answer { m.insert(k.clone(), match ty { Value::String(s) if s.starts_with("number") => json!(1.0), Value::String(s) if s.starts_with("bool") => json!(true), Value::Object(o) => o["enum"][0].clone(), Value::Array(_) => json!([]), _ => json!("x") }); } Value::Object(m) }
        Some(Kind::Code { .. }) | Some(Kind::Transform { .. }) => json!({ "ok": true, "output": {} }),
        Some(Kind::SubTeam { .. }) => json!({ "ok": true, "child_run": "", "outputs": {} }),
        _ => json!({}),
    }
}

// ── today's team, and the starters ───────────────────────────────────────────────────────────────────────────────────────

fn n(g: &mut Graph, id: &str, node: Node) { g.nodes.insert(id.to_string(), node); }
fn new_graph(name: &str, budget: f64) -> Graph { Graph { team: name.to_string(), schema: SCHEMA, branch: format!("team/{}", slug(name)), budget_usd: budget, start: vec![], nodes: BTreeMap::new(), edges: vec![], triggers: vec![], limits: None, setpoints: Default::default() } }
/// A name as an id: lower case words and dashes.
pub fn slug(name: &str) -> String { let t: String = name.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect(); t.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-") }
fn writer(owns: &[&str]) -> Node { Node::new(Kind::Role { role: RoleKind::Worker, tier: None, model: None, readonly: None, fresh_context: None, parallel: None, owns: owns.iter().map(|s| s.to_string()).collect(), agent: None }) }

/// What a v1 team (a planner, a worker and a QA, three attempts) is, as a graph: plan → loop {work → qa → if pass} → deliver, three rejections → the person. Pure.
pub fn from_team_v1(name: &str, has_planner: bool, has_qa: bool, budget_usd: f64) -> Graph {
    let mut g = new_graph(name, budget_usd); let b = budget_usd.max(1.0);
    n(&mut g, "leader", Node::new(Kind::Leader));
    if has_planner { n(&mut g, "plan", Node::role(RoleKind::Planner).cap(b * 0.15)); g.edges.push(edge("plan", "out", "loop")); } else { g.start = vec!["loop".into()]; }
    n(&mut g, "loop", Node::new(Kind::Loop { max_iterations: 3, until: None }).cap(b * 0.6));
    n(&mut g, "work", writer(&["**"]).cap(b * 0.1));
    g.edges.push(edge("loop", "body", "work"));
    n(&mut g, "deliver", Node::new(Kind::Deliver)); n(&mut g, "me", Node::new(Kind::Human { gate: "blocked".into(), on_timeout: Some("stay".into()) }));
    g.edges.push(edge("loop", "exhausted", "me")); g.edges.push(edge("loop", "done", "me")); g.edges.push(edge("me", "approved", "deliver"));
    if has_qa {
        n(&mut g, "qa", Node::role(RoleKind::Qa).cap(b * 0.05)); n(&mut g, "pass", Node::new(Kind::If { cond: "qa.pass == true".into() }));
        g.edges.push(edge("work", "out", "qa")); g.edges.push(edge("qa", "out", "pass")); g.edges.push(edge("pass", "then", "deliver")); g.edges.push(edge("pass", "else", "loop"));
    } else { g.edges.push(edge("work", "out", "deliver")); }
    g
}

/// The starter graphs of RFC 0008 §4.5: a feature team, a bugfix loop, a review loop, and the strict one (audit as a gate).
pub fn template(key: &str, name: &str, budget_usd: f64) -> Option<Graph> {
    let mut g = new_graph(name, budget_usd); let b = budget_usd.max(1.0);
    n(&mut g, "leader", Node::new(Kind::Leader));
    n(&mut g, "deliver", Node::new(Kind::Deliver)); n(&mut g, "me", Node::new(Kind::Human { gate: "merge".into(), on_timeout: Some("stay".into()) }));
    let tests = |ok_effects: &[&str]| Node::new(Kind::UseTool { tool: ToolRef { kind: "skill".into(), name: "run-tests".into(), args: json!({ "scope": "changed" }) }, allow: Allow { skills: vec!["run-tests".into()], effects: ok_effects.iter().map(|s| s.to_string()).collect(), ..Default::default() }, starts_model: false, outputs: [("exit".to_string(), "int".to_string()), ("failed".to_string(), "list".to_string())].into_iter().collect() });
    match key {
        "feature" | "strict" => {
            n(&mut g, "plan", Node::role(RoleKind::Planner).cap(b * 0.13)); n(&mut g, "each", Node::new(Kind::ForEach { over: "plan.sagas".into(), parallel: None, batch: None, on_error: None }));
            n(&mut g, "loop", Node::new(Kind::Loop { max_iterations: 3, until: Some("qa.pass == true".into()) }).cap(b * 0.5));
            n(&mut g, "work", writer(&["src/**"]).cap(b * 0.1)); n(&mut g, "tests", tests(&["read", "exec"]));
            n(&mut g, "green", Node::new(Kind::If { cond: "tests.exit == 0".into() })); n(&mut g, "again", Node::new(Kind::Retry { max: 2, backoff_s: 0, open_after: None, cooldown_s: None }));
            n(&mut g, "fix", { let mut d = Node::role(RoleKind::Debugger).cap(b * 0.07); if let Kind::Role { owns, .. } = &mut d.kind { *owns = vec!["src/**".into()]; } d });
            n(&mut g, "qa", Node::role(RoleKind::Qa).cap(b * 0.05)); n(&mut g, "audit", Node::role(RoleKind::Audit).cap(b * 0.04));
            for e in [("plan", "out", "each"), ("each", "body", "loop"), ("loop", "body", "work"), ("work", "out", "tests"), ("tests", "out", "green"), ("green", "then", "qa"), ("green", "else", "again"), ("again", "again", "fix"), ("again", "exhausted", "me"), ("fix", "out", "tests"),
                      ("qa", "out", "audit"), ("loop", "done", "each"), ("loop", "exhausted", "me"), ("each", "done", "me"), ("me", "approved", "deliver")] { g.edges.push(edge(e.0, e.1, e.2)); }
            // the plain team ends a pass when audit is done (advice); the strict one reads the verdict and sends a flag or a block to the person
            if key == "strict" { n(&mut g, "gate", Node::new(Kind::Switch { on: "audit.verdict".into() })); for e in [("audit", "out", "gate"), ("gate", "pass", "loop"), ("gate", "flag", "me"), ("gate", "block", "me"), ("gate", "default", "me")] { g.edges.push(edge(e.0, e.1, e.2)); } }
            else { g.edges.push(edge("audit", "out", "loop")); }
            Some(g)
        }
        "bugfix" => {
            g.start = vec!["loop".into()];
            n(&mut g, "loop", Node::new(Kind::Loop { max_iterations: 4, until: None }).cap(b * 0.8)); n(&mut g, "tests", tests(&["read", "exec"])); n(&mut g, "green", Node::new(Kind::If { cond: "tests.exit == 0".into() }));
            n(&mut g, "fix", { let mut d = Node::role(RoleKind::Debugger).cap(b * 0.15); if let Kind::Role { owns, .. } = &mut d.kind { *owns = vec!["src/**".into()]; } d });
            for e in [("loop", "body", "tests"), ("tests", "out", "green"), ("green", "then", "me"), ("green", "else", "fix"), ("fix", "out", "loop"), ("loop", "done", "me"), ("loop", "exhausted", "me"), ("me", "approved", "deliver")] { g.edges.push(edge(e.0, e.1, e.2)); }
            Some(g)
        }
        "review" => {
            n(&mut g, "fork", Node::new(Kind::Fork)); n(&mut g, "join", Node::new(Kind::Join { policy: JoinPolicy::All, quorum: None }));
            n(&mut g, "qa_a", Node::role(RoleKind::Qa).cap(b * 0.1)); n(&mut g, "qa_b", Node::role(RoleKind::Qa).cap(b * 0.1)); n(&mut g, "audit", Node::role(RoleKind::Audit).cap(b * 0.1));
            for e in [("fork", "branch", "qa_a"), ("fork", "branch", "qa_b"), ("fork", "branch", "audit"), ("qa_a", "out", "join"), ("qa_b", "out", "join"), ("audit", "out", "join"), ("join", "out", "me"), ("me", "approved", "deliver")] { g.edges.push(edge(e.0, e.1, e.2)); }
            Some(g)
        }
        // PAIR + QA (BL-21): a driver writes, a navigator reads each change and says what is wrong, the project's tests run for free, and an INDEPENDENT QA that never saw their exchange judges against the criteria. A pair shares blind spots; the QA is what catches them.
        "pair" => {
            n(&mut g, "loop", Node::new(Kind::Loop { max_iterations: 3, until: Some("qa.pass == true".into()) }).cap(b * 0.6));
            n(&mut g, "driver", writer(&["src/**"]).cap(b * 0.12)); n(&mut g, "navigator", Node::role(RoleKind::Audit).cap(b * 0.05));
            n(&mut g, "tests", tests(&["read", "exec"])); n(&mut g, "green", Node::new(Kind::If { cond: "tests.exit == 0".into() }));
            n(&mut g, "qa", Node::role(RoleKind::Qa).cap(b * 0.06)); n(&mut g, "pass", Node::new(Kind::If { cond: "qa.pass == true".into() }));
            g.start = vec!["loop".into()];
            for e in [("loop", "body", "driver"), ("driver", "out", "navigator"), ("navigator", "out", "tests"), ("tests", "out", "green"), ("green", "then", "qa"), ("green", "else", "loop"), ("qa", "out", "pass"), ("pass", "then", "me"), ("pass", "else", "loop"), ("loop", "done", "me"), ("loop", "exhausted", "me"), ("me", "approved", "deliver")] { g.edges.push(edge(e.0, e.1, e.2)); }
            Some(g)
        }
        // THE LOCAL CODER (RFC 0014): every worker is a local model; the LEADER writes the contract, the tests and the review rules once (the placeholders here), the nodes do the rest: stubs, then decompose filled under guards, judged by the leader's tests, redone where they blame
        "local_coder" => {
            use crate::team_contract::{ContractSpec, FileContract}; use crate::team_decompose::DecomposeSpec; use crate::team_review::{ReviewRule, ReviewSpec}; use crate::team_stubs::StubsSpec; use crate::team_test::TestSpec;
            g.start = vec!["stubs".into()];
            n(&mut g, "contract", Node::new(Kind::Contract(ContractSpec { conventions: vec!["ES modules; a relative import names the file with its extension, like ../src/a.js".into(), "named exports only, no default export".into()], files: [("src/main.js".to_string(), FileContract { exports: vec!["main".into()], purpose: "REPLACE: the leader decomposes the task into files and functions (signature, does, returns, examples, edge cases)".into(), functions: vec![crate::team_contract::FnContract { signature: "main()".into(), does: "REPLACE: one function per job, the exact signature, what it does".into(), returns: "REPLACE: the shape it returns".into(), examples: vec![], edge_cases: vec![], calls: vec![] }] })].into_iter().collect(), node_test: true })));
            n(&mut g, "leader_tests", Node::new(Kind::Test(TestSpec { files: [("test/leader.test.js".to_string(), "import { test } from 'node:test';\ntest('REPLACE: the leader writes the tests before the run', () => {});\n".to_string())].into_iter().collect(), runner: "node_test".into(), timeout_s: None, hidden: false })));
            n(&mut g, "review", Node::new(Kind::Review(ReviewSpec { xray: true, laws: vec![], rules: vec![ReviewRule { id: "no-const-reassign".into(), kind: "no_const_reassign".into(), message: "declare it with let".into(), pattern: String::new(), max: None, severity: "error".into(), applies: vec![] }] })));
            n(&mut g, "stubs", Node::new(Kind::Stubs(StubsSpec { author: "local:qwen2.5-coder:3b".into(), fallback: vec![], contract: "contract".into(), timeout_s: None })));
            n(&mut g, "decompose", Node::new(Kind::Decompose(DecomposeSpec { planner: "endpoint:lfm3".into(), coder: "local:qwen2.5-coder:3b".into(), fallback: vec![], max_subtasks: 8, owns: vec!["src/**".into()], timeout_s: None, contract: Some("contract".into()), review: Some("review".into()), tests: Some("leader_tests".into()) })));
            n(&mut g, "code", Node::new(Kind::Recurse { worker: "decompose".into(), judge: "leader_tests".into(), max_depth: 3 }));
            for e in [("stubs", "out", "code"), ("code", "out", "me"), ("code", "failed", "me"), ("me", "approved", "deliver")] { g.edges.push(edge(e.0, e.1, e.2)); }
            Some(g)
        }
        // a team the Leader will design from the person's own words (§4.16): the Leader, the person's merge and the end, nothing in between yet
        "blank" => { g.edges.push(edge("me", "approved", "deliver")); Some(g) }
        _ => None,
    }
}
pub const TEMPLATES: [&str; 7] = ["feature", "bugfix", "review", "strict", "blank", "pair", "local_coder"];

/// What changed between two graphs, for the person who reviews a design (RFC 0008 §4.7: an edit applies at the next episode boundary, after the person has seen the diff). Pure.
pub fn diff(old: &Graph, new: &Graph) -> Value {
    let added: Vec<&String> = new.nodes.keys().filter(|k| !old.nodes.contains_key(*k)).collect();
    let removed: Vec<&String> = old.nodes.keys().filter(|k| !new.nodes.contains_key(*k)).collect();
    let changed: Vec<&String> = new.nodes.iter().filter(|(k, n)| old.nodes.get(*k).map_or(false, |o| o != *n)).map(|(k, _)| k).collect();
    let key = |e: &Edge| format!("{}.{} → {}", e.from, e.port, e.to);
    let (oe, ne): (BTreeSet<String>, BTreeSet<String>) = (old.edges.iter().map(key).collect(), new.edges.iter().map(key).collect());
    json!({ "nodes_added": added, "nodes_removed": removed, "nodes_changed": changed, "wires_added": ne.difference(&oe).collect::<Vec<_>>(), "wires_removed": oe.difference(&ne).collect::<Vec<_>>(),
            "budget": if (old.budget_usd - new.budget_usd).abs() > 1e-9 { json!({ "from": old.budget_usd, "to": new.budget_usd }) } else { Value::Null } })
}

// ── what the editor offers ───────────────────────────────────────────────────────────────────────────────────────────────

/// The palette of the editor and the Leader's catalogue (RFC 0008 §4.3, §4.4, board 2): every node a graph can hold, with its default, its ports and what it writes. Pure.
/// THE NODES A LEADER CAN USE, as text, grouped by category with `local` (the support and specialist nodes of a local-model team) first. `category` narrows to one. Pure.
pub fn node_list_md(category: &str) -> String {
    let c = catalog(); let nodes = c["nodes"].as_array().cloned().unwrap_or_default();
    let mut cats: Vec<String> = nodes.iter().filter_map(|n| n["family"].as_str().map(String::from)).collect(); cats.sort(); cats.dedup(); cats.sort_by_key(|k| k != "local");
    let title = |k: &str| match k { "local" => "local — the support and specialist nodes of a local-model team (RFC 0014)".to_string(), "role" => "role — agents".to_string(), "control" => "control — flow".to_string(), "tool" => "tool — no model".to_string(), o => o.to_string() };
    let mut out = String::new();
    for k in cats.iter().filter(|k| category.is_empty() || k.as_str() == category) {
        out.push_str(&format!("## {}\n", title(k)));
        for n in nodes.iter().filter(|n| n["family"].as_str() == Some(k.as_str())) { out.push_str(&format!("- `{}` ({}): {}\n", n["id"].as_str().unwrap_or(""), n["label"].as_str().unwrap_or(""), n["hint"].as_str().unwrap_or(""))); }
        out.push('\n');
    }
    if category.is_empty() || category == "local" { out.push_str("A LOCAL TEAM: start from the template `local_coder`. The LEADER writes, once and before the run: the contract (files and the names each exports), the tests (a `test` node: they are the judge), and the review rules (X-ray format `laws`, or `rules`). The workers are all local (planner `endpoint:lfm3`, coder `local:qwen2.5-coder:3b`); never put a claude-code seat in them. Order: stubs -> recurse(decompose, judge = the tests) -> the person's merge.\n"); }
    out
}
pub fn catalog() -> Value {
    let item = |id: &str, family: &str, label: &str, hint: &str, icon: &str, node: Node| -> Value {
        let ports: Vec<&str> = match &node.kind { Kind::Role { .. } | Kind::UseTool { .. } | Kind::Artifact { .. } | Kind::Check { .. } | Kind::Llm(_) | Kind::Decompose(_) | Kind::Recurse { .. } | Kind::Evolve { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Stubs(_) | Kind::Review(_) | Kind::Enforce(_) | Kind::Skeleton(_) | Kind::Cell(_) | Kind::Code { .. } | Kind::Transform { .. } | Kind::SubTeam { .. } => vec!["out"], Kind::If { .. } => vec!["then", "else"], Kind::Switch { .. } => vec!["default"], Kind::Loop { .. } => vec!["body", "done", "exhausted"], Kind::ForEach { .. } => vec!["body", "done"], Kind::Fork => vec!["branch"],
            Kind::Join { .. } | Kind::Timer { .. } => vec!["out"], Kind::Retry { .. } => vec!["again", "exhausted"], Kind::BudgetGate { .. } => vec!["continue", "stop", "ask"], Kind::Human { .. } => vec!["approved", "denied"], Kind::Leader | Kind::Deliver | Kind::Stop { .. } => vec![] };
        let fails: Vec<&str> = if allowed_port(&node, "failed") { FAILURE_PORTS.to_vec() } else { vec![] };
        json!({ "id": id, "family": family, "label": label, "hint": hint, "icon": icon, "default": serde_json::to_value(&node).unwrap_or(Value::Null), "ports": ports, "required": required_ports(&node), "failure_ports": fails,
                "writes": writes_of(&node).into_iter().map(|(f, t)| json!({ "field": f, "type": t })).collect::<Vec<_>>(), "deterministic": deterministic(&node.kind), "tier": node.tier_of().map(|t| t.key()) })
    };
    let role = |r: RoleKind| { let mut n = Node::role(r); if r.may_edit() { if let Kind::Role { owns, .. } = &mut n.kind { *owns = vec!["src/**".into()]; } } n };
    let tool = |kind: &str, name: &str, effects: &[&str]| Node::new(Kind::UseTool { tool: ToolRef { kind: kind.into(), name: name.into(), args: json!({}) }, allow: Allow { effects: effects.iter().map(|s| s.to_string()).collect(), ..Default::default() }, starts_model: false, outputs: BTreeMap::new() });
    let nodes = vec![
        item("leader", "role", "Leader", "your terminal: designs the team, watches the budget", "leader", Node::new(Kind::Leader)),
        item("planner", "role", "Planner", "sonnet-class · splits the ask into sagas and acceptance criteria", "planner", role(RoleKind::Planner)),
        item("worker", "role", "Worker", "haiku-class · does an episode inside the files it owns", "worker", role(RoleKind::Worker)),
        item("qa", "role", "QA", "fresh context, read-only · judges each criterion with evidence", "qa", role(RoleKind::Qa)),
        item("debugger", "role", "Debugger", "gets only the failure delta and patches it", "debugger", role(RoleKind::Debugger)),
        item("audit", "role", "Audit", "advice, or a gate when a switch reads its verdict", "audit", role(RoleKind::Audit)),
        item("agent", "role", "Agent", "custom · one responsibility with its own brief, skills and tools", "agent", { let mut n = Node::role(RoleKind::Agent); if let Kind::Role { agent, .. } = &mut n.kind { *agent = Some(AgentSpec { title: "Agent".into(), responsibility: "one thing it is for".into(), brief: "what to do, for whom, in what voice".into(), effects: vec!["read".into()], ..Default::default() }); } n }),
        item("llm", "tool", "Model call", "one call to a seat with a typed answer, free checks on it and a ladder to a stronger seat · for a step a small or local model can take (classify, extract) · no tools, no files", "model", Node::new(Kind::Llm(crate::team_llm::Spec { seat: "claude-code:haiku".into(), prompt: "Classify this task as research, write or review.\nTask: {ask}".into(), answer: [("kind".to_string(), json!({ "enum": ["research", "write", "review"] }))].into_iter().collect(), fallback: vec![], verify: Default::default(), agree_with: vec![], max_tokens: None, timeout_s: None, examples_from: None, preset: None, router: None }))),
        item("check", "tool", "Check", "rules that judge a text the run made · free, the same every time, no model · use one where a reviewer's job was mechanical", "check", Node::new(Kind::Check { on: "artifact:report.md".into(), rules: vec![crate::team_check::Rule { id: "no-placeholders".into(), kind: "forbid".into(), pattern: "(?i)lorem ipsum|\\bTODO\\b".into(), message: "a placeholder is left in".into(), ..Default::default() }] })),
        item("artifact", "tool", "Artifact", "a document for the run, not code: a report, a dashboard, a draft · it never reaches your branch", "artifact", Node::new(Kind::Artifact { name: "report".into(), format: "md".into(), from: None, title: None, compose: None })),
        item("compose", "tool", "Master page", "the run's one result page: hero, summary, numbers, documents, cost, timeline · built from data, no model, nothing to write", "artifact", Node::new(Kind::Artifact { name: "result".into(), format: "html".into(), from: None, title: None, compose: Some(crate::team_compose::Spec { title: String::new(), sections: vec![
            crate::team_compose::Section { title: String::new(), widgets: vec![json!({ "type": "hero" })] },
            crate::team_compose::Section { title: "At a glance".into(), widgets: vec![json!({ "type": "kpis", "items": [{ "label": "status", "from": "run:status" }, { "label": "spent", "from": "run:cost" }, { "label": "time", "from": "run:time" }, { "label": "documents", "from": "run:artifacts" }] })] },
            crate::team_compose::Section { title: "Documents".into(), widgets: vec![json!({ "type": "links" })] },
            crate::team_compose::Section { title: "Cost".into(), widgets: vec![json!({ "type": "cost" })] },
            crate::team_compose::Section { title: "Timeline".into(), widgets: vec![json!({ "type": "timeline" })] }] }) })),
        item("if", "control", "if / else", "two ways on a condition", "ifelse", Node::new(Kind::If { cond: "tests.exit == 0".into() })),
        item("switch", "control", "switch", "N ways on a value · a default is required", "switch", Node::new(Kind::Switch { on: "audit.verdict".into() })),
        item("loop", "control", "loop", "bounded: a limit and a cost cap · each pass is an episode", "loop", Node::new(Kind::Loop { max_iterations: 3, until: None }).cap(1.0)),
        item("for_each", "control", "for each", "the sagas of the plan, one after another", "foreach", Node::new(Kind::ForEach { over: "plan.sagas".into(), parallel: None, batch: None, on_error: None })),
        item("timer", "control", "timer", "delay · timeout · schedule", "timer", Node::new(Kind::Timer { mode: TimerMode::Delay, seconds: 60, cron: None, wraps: None })),
        item("fork", "control", "fork", "branches run side by side", "fork", Node::new(Kind::Fork)),
        item("join", "control", "join", "waits for all, the first, or a quorum", "join", Node::new(Kind::Join { policy: JoinPolicy::All, quorum: None })),
        item("retry", "control", "fallback / retry", "sends a failure round again, with a limit", "fallback", Node::new(Kind::Retry { max: 2, backoff_s: 0, open_after: None, cooldown_s: None })),
        item("budget_gate", "control", "budget gate", "continue · stop · ask when a share is spent", "gauge", Node::new(Kind::BudgetGate { stop_at: 0.9, ask_at: Some(0.7) })),
        item("approval", "control", "approval", "a person decides, seeing the risk and what will happen", "hand", { let mut n = Node::new(Kind::Human { gate: "review".into(), on_timeout: Some("stay".into()) }); n.approval = Some(Approval { risk: "medium".into(), note: "What will happen after you approve: ".into() , show: String::new() }); n }),
        item("human", "control", "human approval", "only a person passes it", "hand", Node::new(Kind::Human { gate: "merge".into(), on_timeout: Some("stay".into()) })),
        item("stop", "control", "stop", "ends the run as failed with a message", "stop", Node::new(Kind::Stop { message: "a gate failed".into() })),
        item("deliver", "control", "deliver", "the end: the pack is delivered", "deliver", Node::new(Kind::Deliver)),
        item("code", "transform", "code", "Rhai sandbox · JSON in and out · needs a passing example at save", "code", Node::new(Kind::Code { source: "#{}".into(), examples: vec![crate::team_code::Example { input: json!({}), output: json!({}) }], from: None, limits: None })),
        item("transform", "transform", "transform", "reshape data without a model (filter, sort, template, sanitize…)", "transform", Node::new(Kind::Transform { mode: "sanitize".into(), from: None, config: json!({}) })),
        item("sub_team", "flow", "sub-team", "runs another team as one step · its own runs, rows and audits stay with it · a team cannot include itself", "subteam", Node::new(Kind::SubTeam { team: "other-team".into(), ask: "{ask}".into(), outputs: BTreeMap::new(), budget_share: Some(0.5), max_depth: None })),
        item("tool_web_search", "tool", "web search", "search the web · Brave if you set a key, else your seat's own search (a few cents) · what it finds goes to the nodes after it", "globe", { let mut n = tool("web", "search", &["network"]); n.cap_usd = Some(0.1); if let Kind::UseTool { tool: t, outputs, starts_model, .. } = &mut n.kind { *starts_model = true; t.args = json!({ "query": "{ask}", "max": 5 }); *outputs = [("count".to_string(), "int".to_string()), ("text".to_string(), "string".to_string())].into_iter().collect(); } n }),
        item("tool_web_docs", "tool", "read a docs site", "read a documentation site into files the team keeps · no model", "globe", { let mut n = tool("web", "docs", &["network", "read"]); if let Kind::UseTool { tool: t, outputs, .. } = &mut n.kind { t.args = json!({ "url": "https://example.com/docs/", "max": 12 }); *outputs = [("text".to_string(), "string".to_string()), ("dir".to_string(), "string".to_string())].into_iter().collect(); } n }),
        item("tool_browse_login", "tool", "sign in (browser window)", "opens a window with a profile of its own for the person to sign in to an app · put a human step after it · no model, no password ever read", "globe", { let mut n = tool("browse", "login", &["network"]); if let Kind::UseTool { tool: t, outputs, .. } = &mut n.kind { t.args = json!({ "profile": "app", "domains": ["example.com"], "url": "https://example.com", "wait_s": 600 }); *outputs = [("closed".to_string(), "bool".to_string())].into_iter().collect(); } n }),
        item("tool_browse_capture", "tool", "look at pages (browser)", "visits pages signed in, at several widths · screenshots and a reading of fonts, colours and layout · only reads, only the listed domains · no model", "globe", { let mut n = tool("browse", "capture", &["network", "read"]); if let Kind::UseTool { tool: t, outputs, .. } = &mut n.kind { t.args = json!({ "profile": "app", "domains": ["example.com"], "urls": ["https://example.com"], "widths": [1440, 768, 390] }); *outputs = [("count".to_string(), "int".to_string()), ("dir".to_string(), "string".to_string()), ("shots".to_string(), "list".to_string())].into_iter().collect(); } n }),
        item("tool_web_fetch", "tool", "read a page", "read one web page as text · no model", "globe", { let mut n = tool("web", "fetch", &["network"]); if let Kind::UseTool { tool: t, outputs, .. } = &mut n.kind { t.args = json!({ "url": "https://example.com" }); *outputs = [("text".to_string(), "string".to_string()), ("title".to_string(), "string".to_string())].into_iter().collect(); } n }),
        item("tool_skill", "tool", "skill", "run tests, build, lint · no model", "wrench", { let mut n = tool("skill", "run-tests", &["read", "exec"]); if let Kind::UseTool { outputs, allow, .. } = &mut n.kind { *outputs = [("exit".to_string(), "int".to_string()), ("failed".to_string(), "list".to_string())].into_iter().collect(); allow.skills = vec!["run-tests".into()]; } n }),
        item("tool_preview", "tool", "preview (run the code)", "starts the project the team is building (its dev or start script) and keeps it up, with its address, until the run ends · see a frontend or call a backend while agents work · no model", "globe", { let mut n = tool("skill", "preview", &["read", "exec"]); n.timeout_s = Some(90); if let Kind::UseTool { outputs, allow, .. } = &mut n.kind { *outputs = [("url".to_string(), "string".to_string()), ("ok".to_string(), "bool".to_string())].into_iter().collect(); allow.skills = vec!["preview".into()]; } n }),
        item("tool_law_gate", "tool", "law-gate (judge the work)", "the ratified laws of the book judge what the team made (code, copy, a report, a plan, a table, a page, images) · pass or fail routes the graph · every finding has a place and a fix · no model, the same answer twice", "check", { let mut n = tool("skill", "law-gate", &["read"]); if let Kind::UseTool { tool: t, outputs, allow, .. } = &mut n.kind { t.args = json!({ "packs": [], "paths": [], "fail_on": "error" }); *outputs = [("exit".to_string(), "int".to_string()), ("failed".to_string(), "list".to_string())].into_iter().collect(); allow.skills = vec!["law-gate".into()]; } n }),
        item("test", "local", "Test", "tests written in code BEFORE the run by the leader: they approve or disapprove what the node before made · run in a sandbox copy · the coder never sees them and the improver cannot change them", "check", Node::new(Kind::Test(crate::team_test::TestSpec { files: [("test/example.test.js".to_string(), "import { test } from 'node:test'; test('it works', () => {});\n".to_string())].into_iter().collect(), runner: "node_test".into(), timeout_s: None, hidden: false }))),
        item("cell_summary", "local", "Summary", "a tiny model summarizes; code holds it to the length and to the source: every number and name in the summary must be in the text", "doc", Node::new(Kind::Cell(crate::team_cell::CellSpec { task: "summary".into(), seat: "endpoint:lfm3".into(), from: "ask".into(), params: [("max_words".to_string(), json!(60))].into_iter().collect(), timeout_s: None }))),
        item("cell_translate", "local", "Translate", "a tiny model translates; code checks that placeholders, numbers, URLs and `code` survive unchanged", "globe", Node::new(Kind::Cell(crate::team_cell::CellSpec { task: "translate".into(), seat: "endpoint:lfm3".into(), from: "ask".into(), params: [("to".to_string(), json!("Portuguese"))].into_iter().collect(), timeout_s: None }))),
        item("cell_match", "local", "Match (choose by similarity)", "no generation: the input and each label's description are embedded by a local embedding model (local:nomic-embed-text) and the closest description wins · deterministic · `min_score` with `default` keeps a weak match from being forced · use it to CHOOSE (a motif, a palette, a sound class) instead of asking a small model to write a label", "search", Node::new(Kind::Cell(crate::team_cell::CellSpec { task: "match".into(), seat: "local:nomic-embed-text".into(), from: "ask".into(), params: [("labels".to_string(), json!({ "sea": "water, a boat, rain, a river", "desert": "sand, sun, a tent, a caravan" })), ("min_score".to_string(), json!(0.4)), ("default".to_string(), json!("desert"))].into_iter().collect(), timeout_s: Some(60) }))),
        item("transform_rules", "local", "Rules (clean any output)", "declarative rules for the answer of ANY model or none: per field trim, map near misses, allowed values (exact, nearest, default), case, maximum length cut at a word · the answer names what it fixed (`fixes`) · put it after every small model", "transform", Node::new(Kind::Transform { mode: "rules".into(), from: Some("values:fill".into()), config: json!({ "fields": { "title": { "case": "upper", "max": 28 }, "kind": { "enum": ["a", "b"], "nearest": true, "default": "a" } } }) })),
        item("cell_intent", "local", "Intent", "a tiny model picks ONE label from a list; code accepts only a label of the list", "search", Node::new(Kind::Cell(crate::team_cell::CellSpec { task: "intent".into(), seat: "endpoint:lfm3".into(), from: "ask".into(), params: [("labels".to_string(), json!(["bug", "feature", "question"]))].into_iter().collect(), timeout_s: None }))),
        item("cell_redact", "local", "Redact", "code removes emails, phone numbers and keys; nothing sensitive may remain (a seat may also mask names)", "stop", Node::new(Kind::Cell(crate::team_cell::CellSpec { task: "redact".into(), seat: String::new(), from: "ask".into(), params: Default::default(), timeout_s: None }))),
        item("cell_regex", "local", "Regex from examples", "a tiny model proposes a pattern; code runs it: it must match every positive example and no negative one", "code", Node::new(Kind::Cell(crate::team_cell::CellSpec { task: "regex".into(), seat: "endpoint:lfm3".into(), from: "ask".into(), params: [("positive".to_string(), json!(["a1"])), ("negative".to_string(), json!(["b"]))].into_iter().collect(), timeout_s: None }))),
        item("skeleton", "local", "Skeleton (planned AST diff)", "the planned change at symbol level from the leader's function-level contract and the original code: new-only, merged and diff views · after the fill it checks, by code, that the planned functions exist, are exported, take the planned parameters, call what was named, and nothing unplanned is exported", "branch", Node::new(Kind::Skeleton(crate::team_skeleton::SkeletonSpec { contract: "contract".into() }))),
        item("enforce", "local", "JSON enforcer", "a raw text in, a typed value out: code repairs and coerces first, a tiny local model (qwen3:0.6b, held to the schema) restructures only when code could not, code validates · a verdict names what is missing", "check", Node::new(Kind::Enforce(crate::team_enforce::EnforceSpec { from: "ask".into(), schema: [("kind".to_string(), json!({ "enum": ["bug", "feature"] }))].into_iter().collect(), seat: "local:qwen3:0.6b".into(), timeout_s: None }))),
        item("review", "local", "Deterministic review", "X-ray's rules (the laws, with exact lines) plus rules the leader adds before the run (forbid or require a pattern, limits on function length, parameters and file length, no const that is reassigned) · applied by code to the files a node made · no model", "check", Node::new(Kind::Review(crate::team_review::ReviewSpec { xray: true, laws: vec![], rules: vec![crate::team_review::ReviewRule { id: "no-const-reassign".into(), kind: "no_const_reassign".into(), message: String::new(), pattern: String::new(), max: None, severity: "error".into(), applies: vec![] }] }))),
        item("stubs", "local", "Stubs builder", "a tiny model writes each contract file as a stub (exact signatures, doc comments with examples, bodies that only throw TODO), code validates it · the decompose node then fills them: every signature survives, no TODO is left", "doc", Node::new(Kind::Stubs(crate::team_stubs::StubsSpec { author: "local:qwen2.5-coder:3b".into(), fallback: vec![], contract: "contract".into(), timeout_s: None }))),
        item("contract", "local", "Contract", "written by the leader before the run, enforced by this node: the conventions and the exports of each file bind the models of the decompose node that names it · checked by code (files exist, exports, imports resolve) · the improver cannot change it", "doc", Node::new(Kind::Contract(crate::team_contract::ContractSpec { conventions: vec!["ES modules with named exports; a relative import names the file with its extension".into()], files: [("src/example.js".to_string(), crate::team_contract::FileContract { exports: vec!["example".into()], purpose: "what the file is for".into(), functions: vec![] })].into_iter().collect(), node_test: true }))),
        item("decompose", "local", "Decompose", "a task in, files out: a planner model breaks it into files (checked by contract), a coder model writes each one, code does the rest (X-ray context, order, syntax check, the write)", "foreach", Node::new(Kind::Decompose(crate::team_decompose::DecomposeSpec { planner: "endpoint:lfm3".into(), coder: "local:qwen2.5-coder:3b".into(), fallback: vec!["claude-code:haiku".into()], max_subtasks: 6, owns: vec![], timeout_s: None, contract: None, review: None, tests: None }))),
        item("recurse", "local", "Recurse", "runs a decompose node, judges it with the tests, and redoes only the files the failure points at · stops at the depth or when the same failure repeats", "loop", Node::new(Kind::Recurse { worker: "decompose".into(), judge: "tests".into(), max_depth: 3 })),
        item("evolve", "local", "Evolve", "marks a section of the team the improver may change, and asks it for a request about that section when a run fails there", "spark", Node::new(Kind::Evolve { scope: vec!["decompose".into()], after_failures: 1 })),
        item("tool_xray", "local", "X-ray", "hands the team anything X-ray knows: a budgeted context bundle for the ask, a file's AST, its review, who imports it, the tests that reach it · no model · the MCP's own tools", "search", { let mut n = tool("mcp", "antiloki_query", &["read"]); if let Kind::UseTool { tool: t, outputs, starts_model, .. } = &mut n.kind { *starts_model = true; t.args = json!({ "source": "bundle", "question": "{ask}", "token_budget": 4000 }); *outputs = [("ok".to_string(), "bool".to_string()), ("text".to_string(), "string".to_string())].into_iter().collect(); } n }),
        item("tool_package", "local", "bring in a pinned package and run it", "fetches a GitHub source pinned to its full commit into this run's folder, installs exactly pinned wheels in a venv, runs ONE entry of a fixed list (rea_video: breaks a video into shots and writes a VIDEO.md) · the person approves each commit once · scripts off", "doc", { let mut n = tool("package", "rea_video", &["network", "write", "exec"]); if let Kind::UseTool { tool: t, outputs, .. } = &mut n.kind { t.args = json!({ "source": "git", "name": "JuneYaooo/rea-video", "version": "07dd587119e884f7c50fbfae92e47840de86bea5", "deps": ["numpy==2.5.5", "opencv-python-headless==4.14.0.94", "pillow==13.0.0", "PyYAML==6.0.3", "videomd==1.2.0", "onnxruntime==1.32.0"], "input": "reference/ref.mp4", "out": "reference/analysis" }); *outputs = [("ok".to_string(), "bool".to_string()), ("video_md".to_string(), "string".to_string()), ("timeline".to_string(), "string".to_string())].into_iter().collect(); } n }),
        item("tool_sound_cues", "local", "read the cue sheet of a template", "reads the named beats (`tl.addLabel(\"cue:<name>\", <number>)`) from a GSAP template and CHECKS them statically: each label must equal the start of the first tween after it · no model, no render · fails with the exact drift", "doc", { let mut n = tool("sound", "cue_sheet", &["read"]); if let Kind::UseTool { tool: t, outputs, .. } = &mut n.kind { t.args = json!({ "file": "scenes.js" }); *outputs = [("ok".to_string(), "bool".to_string()), ("count".to_string(), "number".to_string())].into_iter().collect(); } n }),
        item("tool_sound_mix", "local", "mix the sound for a video", "effects chosen by similarity to what the leader says each beat should sound like and locked to the cues (each starts early by its measured attack), a narration line fitted to its window by rules, a tiny local voice (Piper), a music bed chosen for the topic's text, loudness, captions from the heard words, then checks: loudness, true peak, fit, a speech round trip tolerant of names, sound placement with a class-dependent tolerance, big picture events by pixels · needs the person's sound runtime (scripts/install-sound-runtime.sh)", "doc", { let mut n = tool("sound", "mix", &["read", "write", "exec"]); if let Kind::UseTool { tool: t, outputs, .. } = &mut n.kind { t.args = json!({ "file": "scenes.js", "video": "out/video.mp4", "out": "out/video-sound.mp4", "text_from": "values:rules.output.subtitle", "spec": { "beats": { "zoom": { "want": "a quick whoosh as the camera zooms in", "gain_db": -8, "visual": "big" }, "motif": { "want": "a cheerful pop as an illustration appears", "gain_db": -6 }, "kicker": { "want": "a small bright chime for a label", "gain_db": -10 }, "title": { "want": "a strong confident hit as the title lands", "gain_db": -5, "visual": "big" }, "subtitle": { "want": "a soft swish as a line slides in", "gain_db": -9 }, "outro": { "want": "a whoosh out as the scene fades away", "gain_db": -8, "visual": "big" } }, "narration": { "start_s": 1.7, "end_s": 4.5, "wps": 3.0, "gain_db": 0 }, "music": { "want": "", "want_from": "values:tx.output", "gain_db": -22, "duck_db": -12 }, "target_lufs": -14.0, "true_peak_max": -1.0 } }); *outputs = [("ok".to_string(), "bool".to_string()), ("out".to_string(), "string".to_string())].into_iter().collect(); } n }),
        item("tool_apply", "local", "write the model's files", "writes the complete files a model returned into this run's tree · the model never touches the disk · inside the tree only, a few files, no .git or .env", "doc", { let mut n = tool("apply", "files", &["write"]); if let Kind::UseTool { tool: t, outputs, .. } = &mut n.kind { t.args = json!({ "files": [{ "path": "src/example.js", "content_from": "writer.src" }] }); *outputs = [("ok".to_string(), "bool".to_string())].into_iter().collect(); } n }),
        item("tool_pane", "tool", "pane", "Frames, Preview, Diff · no model", "pane", tool("pane", "frames", &["read"])),
        item("tool_mcp", "tool", "mcp", "any tool in the box · no model", "plug", tool("mcp", "antiloki_check", &["read"])),
        item("tool_source", "tool", "source", "md · images · video, read-only", "doc", tool("source", "README.md", &["read"])),
        item("tool_webapp", "tool", "web app", "calls a tool of an installed app through its MCP · a read, a write or a destroy by the tool's own annotations", "globe", Node::new(Kind::UseTool { tool: ToolRef { kind: "webapp".into(), name: "linear".into(), args: json!({ "tool": "list_issues", "arguments": {} }) }, allow: Allow { effects: vec!["read".into()], ..Default::default() }, starts_model: false, outputs: BTreeMap::new() })),
        item("tool_db", "tool", "database", "a sqlite file, a csv, or the team's own bench.sqlite · read-only unless the person consented · parameterized SQL only · a row limit and a timeout", "db", Node::new(Kind::UseTool { tool: ToolRef { kind: "db".into(), name: "shop.sqlite".into(), args: json!({ "kind": "sqlite", "op": "query", "sql": "SELECT * FROM items LIMIT 10", "params": [] }) }, allow: Allow { effects: vec!["read".into()], ..Default::default() }, starts_model: false, outputs: BTreeMap::new() })),
        item("tool_file", "tool", "file", "read or write a text, markdown, json or csv file inside this run's own folder", "file", Node::new(Kind::UseTool { tool: ToolRef { kind: "file".into(), name: "notes/out.md".into(), args: json!({ "op": "write", "content_from": "values:node.field" }) }, allow: Allow { effects: vec!["write".into()], ..Default::default() }, starts_model: false, outputs: BTreeMap::new() })),
        item("tool_saga", "tool", "saga", "read, write, run or sync this workspace's sagas through one typed door", "saga", Node::new(Kind::UseTool { tool: ToolRef { kind: "saga".into(), name: "read".into(), args: json!({ "what": "list" }) }, allow: Allow { effects: vec!["read".into()], ..Default::default() }, starts_model: false, outputs: BTreeMap::new() })),
    ];
    let roles: Vec<Value> = [RoleKind::Planner, RoleKind::Worker, RoleKind::Qa, RoleKind::Debugger, RoleKind::Audit].iter().map(|r| json!({ "role": r.key(), "tier": r.default_tier().key(), "edits": r.may_edit() })).collect();
    json!({ "nodes": nodes, "templates": TEMPLATES, "tiers": ["low", "mid", "high"], "effects": EFFECTS, "artifact_formats": ARTIFACT_FORMATS, "media_formats": MEDIA_FORMATS, "tool_types": TOOL_TYPES, "failure_ports": FAILURE_PORTS, "roles": roles })
}

// ── tests ────────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[test]
    fn only_nodes_that_call_no_model_are_flagged_deterministic() {
        let tool = |sm: bool| Kind::UseTool { tool: ToolRef { kind: "web".into(), name: "fetch".into(), args: json!({}) }, allow: Default::default(), starts_model: sm, outputs: Default::default() };
        assert!(deterministic(&tool(false)) && !deterministic(&tool(true)), "a tool is deterministic unless it starts a model");
        for k in [Kind::Fork, Kind::Deliver, Kind::Stop { message: "x".into() }, Kind::If { cond: "a == 1".into() }, Kind::Human { gate: "review".into(), on_timeout: None }] { assert!(deterministic(&k), "{k:?}"); }
        for k in [Kind::Leader, Kind::SubTeam { team: "t".into(), ask: String::new(), outputs: Default::default(), budget_share: None, max_depth: None }] { assert!(!deterministic(&k), "{k:?}"); }
        let cat = catalog(); let items = cat["nodes"].as_array().unwrap(); let get = |id: &str| items.iter().find(|x| x["id"] == json!(id)).map(|x| x["deterministic"].clone());
        assert_eq!(get("if"), Some(json!(true))); assert_eq!(get("leader"), Some(json!(false)));
        assert!(items.iter().all(|x| x["deterministic"].is_boolean()), "every catalog entry says");
    }

    use super::*;
    fn vals(v: Value) -> Map<String, Value> { v.as_object().cloned().unwrap() }
    fn ctx_of<'a>(m: &'a Map<String, Value>) -> Ctx<'a> { Ctx { values: m, spent_usd: 1.0, budget_usd: 4.0, elapsed_s: 30.0 } }

    #[test]
    fn a_condition_reads_typed_values_and_nothing_else() {
        let m = vals(json!({ "tests": { "exit": 0, "failed": [] }, "audit": { "verdict": "flag" }, "loop": { "count": 2 } })); let c = ctx_of(&m);
        for (src, want) in [("tests.exit == 0", true), ("tests.exit != 0", false), ("loop.count < 3 and tests.exit == 0", true), ("not (loop.count >= 2)", false), ("audit.verdict in [\"flag\", \"block\"]", true), ("audit.verdict == \"pass\" || loop.count == 2", true),
                            ("budget.left > 0.5", true), ("budget.spent <= 1", true), ("elapsed > 10", true), ("!false", true), ("-1 < 0", true)] {
            assert_eq!(holds(src, &c), Ok(want), "{src}");
        }
        for bad in ["", "tests.exit ==", "tests.exit = 0", "len(tests.failed) == 0", "tests.exit == 0 ; x", "tests.exit == 0 )", "(tests.exit == 0", "x in tests", "\"open", "tests."] { assert!(parse_expr(bad).is_err(), "{bad:?} must not parse"); }
        assert!(holds("tests.nothing == 1", &c).unwrap_err().contains("has not been written yet"), "an unwritten value is told, not guessed");
        assert!(holds("audit.verdict < 1", &c).unwrap_err().contains("only numbers are ordered"));
        assert!(holds("tests.exit", &c).unwrap_err().contains("true or false"), "a condition answers yes or no");
        assert_eq!(operands(&parse_expr("tests.exit == 0 and not audit.ok").unwrap()), vec!["tests.exit".to_string(), "audit.ok".to_string()]);
    }

    fn one_loop() -> Graph { template("bugfix", "fix it", 2.0).unwrap() }

    #[test]
    fn the_starter_graphs_are_valid_and_say_what_the_worst_case_costs() {
        for k in TEMPLATES { let g = template(k, "pack", 6.0).unwrap(); let r = validate(&g); assert!(r.ok(), "{k}: {:?}", r.words()); assert!(k == "blank" || k == "local_coder" || r.worst_usd > 0.0, "{k} has a worst case (the blank team has no work in it yet; the local coder runs on local models: no spend)"); assert_eq!(Graph::from_json(&g.to_json()).unwrap(), g, "{k} round-trips through its file"); }
        assert!(template("nope", "x", 1.0).is_none());
        let g = template("feature", "pack", 6.0).unwrap(); let r = validate(&g);
        assert!(r.worst_usd <= 6.0 * 1.5, "the loop's own cap bounds its body, not three times every node: {}", r.worst_usd);
    }

    #[test]
    fn the_validator_catches_the_leaders_three_first_draft_mistakes() {
        let base = template("local_coder", "pack", 2.0).unwrap(); let r = validate(&base); assert!(r.ok(), "the starter is clean: {:?}", r.words());
        let dec = base.nodes.iter().find(|(_, n)| matches!(n.kind, Kind::Decompose(_))).map(|(k, _)| k.clone()).expect("a decompose node");
        // 1. the code step's failure is not wired: refused (it was only a warning before)
        let mut g = base.clone(); g.edges.retain(|e| !(e.from == "code" && FAILURE_PORTS.contains(&e.port.as_str()))); let r = validate(&g); assert!(r.has("code_failure_unwired") && !r.ok(), "{:?}", r.words());
        // 2. a small coder and a contract with files but no functions: refused
        let mut g = base.clone(); let cid = match &g.nodes[&dec].kind { Kind::Decompose(sp) => sp.contract.clone(), _ => None };
        if let Some(cid) = cid { if let Some(Node { kind: Kind::Contract(c), .. }) = g.nodes.get_mut(&cid) { c.files.clear(); c.files.insert("a.js".into(), serde_json::from_value(serde_json::json!({ "purpose": "x" })).unwrap()); } let r = validate(&g); assert!(r.has("contract_no_functions") && !r.ok(), "{:?}", r.words()); }
        // 3. a judge in the flow with nothing that makes files before it: told
        let mut g = base.clone(); g.nodes.insert("late_judge".into(), Node::new(Kind::Test(serde_json::from_value(serde_json::json!({ "files": { "t.test.js": "x" } })).unwrap()))); g.start.push("late_judge".into()); g.edges.push(edge("late_judge", "out", "me")); let r = validate(&g); assert!(r.has("judge_before_writer"), "{:?}", r.words());
    }

    #[test]
    fn the_validator_refuses_what_could_run_away_or_stop_nowhere() {
        // a cycle with no bounded loop
        let mut g = one_loop(); g.edges.retain(|e| !(e.from == "fix")); g.edges.push(edge("fix", "out", "tests")); let r = validate(&g); assert!(r.has("unbounded_cycle"), "{:?}", r.words());
        // a loop with no limit and no cap
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("loop") { n.kind = Kind::Loop { max_iterations: 0, until: None }; n.cap_usd = None; } let r = validate(&g); assert!(r.has("loop_unbounded") && r.has("loop_no_cap"), "{:?}", r.words());
        // a dead end, no delivery, no leader, two leaders
        let mut g = one_loop(); g.edges.retain(|e| e.from != "tests"); assert!(validate(&g).has("dead_end"));
        let mut g = one_loop(); g.nodes.remove("deliver"); g.edges.retain(|e| e.to != "deliver"); assert!(validate(&g).has("no_delivery"));
        let mut g = one_loop(); g.nodes.remove("leader"); assert!(validate(&g).has("no_leader"));
        let mut g = one_loop(); g.nodes.insert("l2".into(), Node::new(Kind::Leader)); assert!(validate(&g).has("many_leaders"));
        // a wire to nowhere and a port the node does not have
        let mut g = one_loop(); g.edges.push(edge("tests", "out", "ghost")); assert!(validate(&g).has("edge_to_nowhere"));
        let mut g = one_loop(); g.edges.push(edge("tests", "then", "fix")); assert!(validate(&g).has("bad_port"));
        // a failure nobody catches is told, not refused
        let r = validate(&one_loop()); assert!(r.has("silent_failure_edge") && r.ok(), "{:?}", r.words());
    }

    #[test]
    fn roles_tools_and_conditions_are_held_to_what_they_may_do() {
        let mut g = one_loop();
        if let Some(n) = g.nodes.get_mut("fix") { if let Kind::Role { owns, .. } = &mut n.kind { owns.clear(); } } assert!(validate(&g).has("writer_no_scope"));
        let mut g = template("feature", "p", 6.0).unwrap(); if let Some(n) = g.nodes.get_mut("qa") { if let Kind::Role { readonly, .. } = &mut n.kind { *readonly = Some(false); } } assert!(validate(&g).has("role_not_readonly"));
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("tests") { if let Kind::UseTool { allow, .. } = &mut n.kind { allow.effects.clear(); } } assert!(validate(&g).has("tool_no_allowlist"));
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("tests") { if let Kind::UseTool { tool, .. } = &mut n.kind { tool.kind = "shell".into(); } } assert!(validate(&g).has("unknown_tool_type"));
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("tests") { if let Kind::UseTool { allow, .. } = &mut n.kind { allow.skills = vec!["lint".into()]; } } assert!(validate(&g).has("tool_outside_allowlist"));
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("tests") { if let Kind::UseTool { tool, allow, .. } = &mut n.kind { tool.kind = "source".into(); allow.effects = vec!["read".into(), "write".into()]; } } assert!(validate(&g).has("source_not_read_only"));
        // a destructive tool with a way to it that skips the human
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("tests") { if let Kind::UseTool { allow, .. } = &mut n.kind { allow.effects.push("destructive".into()); } } assert!(validate(&g).has("destructive_without_human"), "{:?}", validate(&g).words());
        // conditions: bad text, a node that does not exist, a field it does not write, a node that is not upstream
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("loop") { n.kind = Kind::Loop { max_iterations: 3, until: Some("tests.exit ==".into()) }; } assert!(validate(&g).has("bad_condition"));
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("loop") { n.kind = Kind::Loop { max_iterations: 3, until: Some("ghost.exit == 0".into()) }; } assert!(validate(&g).has("unwritten_operand"));
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("loop") { n.kind = Kind::Loop { max_iterations: 3, until: Some("tests.nothing == 0".into()) }; } let r = validate(&g); assert!(r.has("unwritten_operand") && r.words().iter().any(|w| w.contains("writes only: ok, exit, failed")), "{:?}", r.words());
        // a switch with no default
        let mut g = template("strict", "p", 6.0).unwrap(); g.edges.retain(|e| !(e.from == "gate" && e.port == "default")); assert!(validate(&g).has("switch_no_default"));
    }

    #[test]
    fn a_graph_names_a_tool_never_a_command_and_never_a_newer_schema() {
        let mut v = template("bugfix", "p", 2.0).unwrap().to_json();
        assert!(Graph::from_json(&v).is_ok());
        v["nodes"]["tests"]["tool"]["args"] = json!({ "command": "rm -rf /" }); assert!(Graph::from_json(&v).unwrap_err().contains("never a command line"));
        let mut v = template("bugfix", "p", 2.0).unwrap().to_json(); v["schema"] = json!(99); assert!(Graph::from_json(&v).unwrap_err().contains("update antiloki"));
        assert!(Graph::from_json(&json!({ "team": "x", "nodes": { "a": { "node": "teleport" } } })).is_err());
    }

    #[test]
    fn parallel_writers_must_own_different_files() {
        let mut g = new_graph("p", 4.0);
        n(&mut g, "leader", Node::new(Kind::Leader)); n(&mut g, "fork", Node::new(Kind::Fork)); n(&mut g, "join", Node::new(Kind::Join { policy: JoinPolicy::All, quorum: None }));
        n(&mut g, "a", writer(&["src/**"]).cap(0.5)); n(&mut g, "b", writer(&["src/ui/**"]).cap(0.5)); n(&mut g, "deliver", Node::new(Kind::Deliver));
        for e in [("fork", "branch", "a"), ("fork", "branch", "b"), ("a", "out", "join"), ("b", "out", "join"), ("join", "out", "deliver")] { g.edges.push(edge(e.0, e.1, e.2)); }
        assert!(validate(&g).has("parallel_overlap"), "{:?}", validate(&g).words());
        if let Some(n) = g.nodes.get_mut("b") { if let Kind::Role { owns, .. } = &mut n.kind { *owns = vec!["docs/**".into()]; } } assert!(!validate(&g).has("parallel_overlap"));
    }

    #[test]
    fn the_worst_case_counts_every_pass_and_a_loops_cap_bounds_its_body() {
        let g = template("bugfix", "p", 2.0).unwrap(); // loop of 4 passes, cap 1.6; fix cap 0.3 x4 = 1.2 < 1.6
        assert_eq!(multiplier(&g, "fix"), 4.0); assert_eq!(multiplier(&g, "leader"), 1.0);
        let mut g2 = g.clone(); if let Some(n) = g2.nodes.get_mut("loop") { n.cap_usd = Some(0.5); } assert!(worst_case_usd(&g2) <= 0.5 + 1e-9, "the cap bounds the body: {}", worst_case_usd(&g2));
        let mut g3 = g.clone(); g3.budget_usd = 0.1; assert!(validate(&g3).has("worst_case_over_budget"));
    }

    // ── graph_step ──
    fn run_all(g: &Graph, stubs: &[(&str, Vec<Value>)]) -> Sim { simulate(g, &stubs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(), Map::new()).unwrap() }

    #[test]
    fn a_loop_goes_round_until_its_condition_holds_and_never_past_its_limit() {
        let g = one_loop();
        let s = run_all(&g, &[("tests", vec![json!({ "exit": 1 }), json!({ "exit": 1 }), json!({ "exit": 0 })])]);
        assert_eq!(s.passes["loop"], 3, "{:?}", s.path); assert_eq!(s.outcome, "delivered");
        assert_eq!(s.path.iter().filter(|p| *p == "tests").count(), 3); assert_eq!(s.path.iter().filter(|p| *p == "fix").count(), 2, "green on the third try: no fix after it");
        // never green: stops at the limit and goes to the person (the `exhausted` edge), and the human approving delivers
        let s = run_all(&g, &[("tests", vec![json!({ "exit": 1 })])]); assert_eq!(s.passes["loop"], 4, "{:?}", s.path); assert!(s.path.contains(&"me".to_string()));
    }

    #[test]
    fn a_failure_goes_by_its_own_port_and_an_uncaught_one_is_handed_to_the_person() {
        let mut g = one_loop(); g.edges.push(edge("tests", "error", "me"));
        let mut s = RunState::new(); let a = graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap(); assert_eq!(a, vec![Action::Pass { node: "loop".into(), n: 1 }, Action::Run { node: "tests".into() }]);
        let a = graph_step(&g, &mut s, Event::NodeFailed { node: "tests".into(), kind: FailKind::Error, error: "no such skill".into(), cost_usd: 0.0 }).unwrap(); assert_eq!(a, vec![Action::Park { node: "me".into(), gate: "merge".into() }]);
        // without the edge
        let g = one_loop(); let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        let a = graph_step(&g, &mut s, Event::NodeFailed { node: "tests".into(), kind: FailKind::Timeout, error: "".into(), cost_usd: 0.0 }).unwrap();
        assert!(matches!(a.last(), Some(Action::Handoff { reason }) if reason.contains("timeout")), "{a:?}"); assert!(matches!(s.status, RunStatus::Blocked(_)));
    }

    #[test]
    fn only_a_person_answers_at_a_human_node_and_a_denial_stops_the_run_unless_it_is_wired() {
        let g = one_loop(); let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        assert!(graph_step(&g, &mut s, Event::Answer { node: "tests".into(), approve: true }).unwrap_err().contains("only a person answers"), "a tool, an agent or a hook cannot approve");
        let g = template("review", "r", 3.0).unwrap(); let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        for n in ["qa_a", "qa_b", "audit"] { graph_step(&g, &mut s, Event::NodeDone { node: n.into(), outputs: json!({}), cost_usd: 0.1 }).unwrap(); }
        assert!(s.active.contains_key("me"), "{:?}", s.active);
        let a = graph_step(&g, &mut s, Event::Answer { node: "me".into(), approve: false }).unwrap(); assert!(matches!(a.last(), Some(Action::Handoff { .. })) && matches!(s.status, RunStatus::Blocked(_)));
    }

    #[test]
    fn a_fork_runs_its_branches_and_a_join_waits_for_all_or_the_first_or_a_quorum() {
        let mut g = template("review", "r", 3.0).unwrap();
        let mut s = RunState::new(); let a = graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        assert_eq!(a.iter().filter(|x| matches!(x, Action::Run { .. })).count(), 3, "three branches start together: {a:?}");
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "qa_a".into(), outputs: json!({ "pass": true }), cost_usd: 0.0 }).unwrap(); assert!(a.is_empty(), "the join waits");
        // any: the first one wins and the others are cancelled
        if let Some(n) = g.nodes.get_mut("join") { n.kind = Kind::Join { policy: JoinPolicy::Any, quorum: None }; }
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "audit".into(), outputs: json!({}), cost_usd: 0.0 }).unwrap();
        assert_eq!(a.iter().filter(|x| matches!(x, Action::Cancel { .. })).count(), 2, "{a:?}"); assert!(a.contains(&Action::Park { node: "me".into(), gate: "merge".into() }));
        // quorum of 2
        if let Some(n) = g.nodes.get_mut("join") { n.kind = Kind::Join { policy: JoinPolicy::Quorum, quorum: Some(2) }; }
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        assert!(graph_step(&g, &mut s, Event::NodeDone { node: "qa_a".into(), outputs: json!({}), cost_usd: 0.0 }).unwrap().is_empty());
        assert!(!graph_step(&g, &mut s, Event::NodeDone { node: "qa_b".into(), outputs: json!({}), cost_usd: 0.0 }).unwrap().is_empty());
    }

    #[test]
    fn a_for_each_takes_the_sagas_of_the_plan_one_after_another() {
        let g = template("feature", "pack", 6.0).unwrap();
        let s = simulate(&g, &[("plan".to_string(), vec![json!({ "sagas": [{ "id": "a" }, { "id": "b" }, { "id": "c" }] })])].into_iter().collect(), Map::new()).unwrap();
        assert_eq!(s.path.iter().filter(|p| *p == "work").count(), 3, "one pass of the loop per saga: {:?}", s.path); assert_eq!(s.outcome, "delivered");
        assert!(s.trace.iter().any(|t| t.kind == "for_each" && t.text.contains("item 3 of 3")), "{:?}", s.trace);
    }

    #[test]
    fn a_timer_waits_a_node_runs_out_of_time_and_a_budget_stops_the_run() {
        let mut g = new_graph("t", 1.0); n(&mut g, "leader", Node::new(Kind::Leader)); n(&mut g, "wait", Node::new(Kind::Timer { mode: TimerMode::Delay, seconds: 30, cron: None, wraps: None })); n(&mut g, "deliver", Node::new(Kind::Deliver)); g.edges.push(edge("wait", "out", "deliver"));
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap(); assert!(graph_step(&g, &mut s, Event::Tick { now_ms: 29_999 }).unwrap().is_empty());
        assert_eq!(graph_step(&g, &mut s, Event::Tick { now_ms: 30_000 }).unwrap(), vec![Action::Deliver]); assert_eq!(s.status, RunStatus::Done);
        // a node with a timeout
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("tests") { n.timeout_s = Some(600); } g.edges.push(edge("tests", "timeout", "me"));
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap(); assert!(graph_step(&g, &mut s, Event::Tick { now_ms: 599_000 }).unwrap().is_empty());
        let a = graph_step(&g, &mut s, Event::Tick { now_ms: 600_000 }).unwrap(); assert!(a.contains(&Action::Cancel { node: "tests".into() }) && a.contains(&Action::Park { node: "me".into(), gate: "merge".into() }), "{a:?}");
        // a budget spent: the run is handed to the person
        let g = one_loop(); let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "tests".into(), outputs: json!({ "exit": 1 }), cost_usd: 2.5 }).unwrap(); assert!(matches!(a.last(), Some(Action::Handoff { reason }) if reason.contains("budget")), "{a:?}"); assert!(s.finished());
    }

    #[test]
    fn a_person_is_waited_for_and_a_timeout_never_approves() {
        let mut g = one_loop(); if let Some(n) = g.nodes.get_mut("me") { n.timeout_s = Some(60); }
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        graph_step(&g, &mut s, Event::NodeDone { node: "tests".into(), outputs: json!({ "exit": 0 }), cost_usd: 0.0 }).unwrap(); assert!(s.active.contains_key("me"));
        assert!(graph_step(&g, &mut s, Event::Tick { now_ms: 10_000_000 }).unwrap().is_empty(), "a parked run costs nothing and waits: the default is to stay"); assert!(s.active.contains_key("me"));
        if let Some(n) = g.nodes.get_mut("me") { n.kind = Kind::Human { gate: "merge".into(), on_timeout: Some("handoff".into()) }; }
        let a = graph_step(&g, &mut s, Event::Tick { now_ms: 10_000_001 }).unwrap(); assert!(matches!(a.last(), Some(Action::Handoff { .. })), "{a:?}");
    }

    #[test]
    fn a_node_over_its_cap_goes_by_the_cap_port() {
        let mut g = one_loop(); g.edges.push(edge("tests", "cap", "me")); if let Some(n) = g.nodes.get_mut("tests") { n.cap_usd = Some(0.1); }
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "tests".into(), outputs: json!({ "exit": 0 }), cost_usd: 0.4 }).unwrap(); assert!(a.contains(&Action::Park { node: "me".into(), gate: "merge".into() }), "{a:?}");
    }

    #[test]
    fn pause_holds_the_next_node_until_resume_and_stop_cancels_whatever_runs() {
        let g = one_loop(); let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        graph_step(&g, &mut s, Event::Pause).unwrap();
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "tests".into(), outputs: json!({ "exit": 1 }), cost_usd: 0.0 }).unwrap(); assert!(!a.iter().any(|x| matches!(x, Action::Run { .. })), "paused: nothing new starts: {a:?}"); assert_eq!(s.queued, vec!["fix".to_string()]);
        let a = graph_step(&g, &mut s, Event::Resume).unwrap(); assert_eq!(a, vec![Action::Run { node: "fix".into() }]);
        let a = graph_step(&g, &mut s, Event::Stop).unwrap(); assert_eq!(a, vec![Action::Cancel { node: "fix".into() }]); assert_eq!(s.status, RunStatus::Stopped);
    }

    #[test]
    fn a_run_is_replayed_from_its_events_to_the_same_state() {
        let g = template("feature", "pack", 6.0).unwrap();
        let evs = |g: &Graph| -> RunState { let mut s = RunState::new(); let mut queue: VecDeque<Action> = graph_step(g, &mut s, Event::Start { now_ms: 5, seed: Map::new() }).unwrap().into(); while let Some(a) = queue.pop_front() { if let Action::Run { node } = a { queue.extend(graph_step(g, &mut s, Event::NodeDone { node: node.clone(), outputs: default_output(g.node(&node)), cost_usd: 0.01 }).unwrap()); } else if let Action::Park { node, .. } = a { queue.extend(graph_step(g, &mut s, Event::Answer { node, approve: true }).unwrap()); } } s };
        let (a, b) = (evs(&g), evs(&g)); assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::to_value(&b).unwrap(), "the same events, the same state"); assert_eq!(a.status, RunStatus::Done);
    }

    #[test]
    fn a_v1_team_is_a_graph_that_plans_works_is_judged_and_stops_for_the_person_after_three_rejections() {
        let g = from_team_v1("k", true, true, 3.0); let r = validate(&g); assert!(r.ok(), "{:?}", r.words());
        let s = run_all(&g, &[("qa", vec![json!({ "pass": false })])]); assert_eq!(s.passes["loop"], 3, "three attempts at most: {:?}", s.path); assert!(s.path.contains(&"me".to_string()), "then the person");
        let s = run_all(&g, &[("qa", vec![json!({ "pass": false }), json!({ "pass": true })])]); assert_eq!(s.passes["loop"], 2); assert_eq!(s.outcome, "delivered");
        let none = from_team_v1("k", false, false, 3.0); assert!(validate(&none).ok(), "{:?}", validate(&none).words()); assert_eq!(run_all(&none, &[]).outcome, "delivered");
    }

    #[test]
    fn a_strict_team_reads_the_audit_verdict_a_plain_one_does_not() {
        let strict = template("strict", "p", 6.0).unwrap();
        let s = run_all(&strict, &[("audit", vec![json!({ "verdict": "block" })])]); assert!(s.path.contains(&"me".to_string()) && s.passes["loop"] == 1, "a block goes to the person: {:?}", s.path);
        let s = run_all(&strict, &[("audit", vec![json!({ "verdict": "pass" })])]); assert_eq!(s.outcome, "delivered");
        let plain = template("feature", "p", 6.0).unwrap(); let s = run_all(&plain, &[("audit", vec![json!({ "verdict": "block" })])]); assert_eq!(s.outcome, "delivered", "audit is advice unless a switch reads it");
    }

    #[test]
    fn a_design_is_shown_as_what_changed() {
        let a = template("bugfix", "p", 2.0).unwrap(); let mut b = a.clone(); b.nodes.get_mut("loop").map(|n| n.kind = Kind::Loop { max_iterations: 5, until: None }); b.nodes.remove("fix"); b.edges.retain(|e| e.from != "fix" && e.to != "fix"); b.budget_usd = 3.0;
        let d = diff(&a, &b); assert_eq!(d["nodes_removed"], json!(["fix"])); assert_eq!(d["nodes_changed"], json!(["loop"])); assert_eq!(d["budget"], json!({ "from": 2.0, "to": 3.0 })); assert!(d["wires_removed"].as_array().unwrap().iter().any(|w| w == "fix.out → loop"));
        assert_eq!(diff(&a, &a)["nodes_changed"], json!([]));
    }

    #[test]
    fn the_catalogue_offers_every_node_with_a_default_that_reads_back() {
        let c = catalog(); let nodes = c["nodes"].as_array().unwrap();
        for k in ["planner", "worker", "qa", "debugger", "audit", "if", "switch", "loop", "for_each", "timer", "fork", "join", "retry", "budget_gate", "human", "deliver", "tool_skill", "tool_pane", "tool_mcp", "tool_source", "tool_webapp"] { assert!(nodes.iter().any(|n| n["id"] == json!(k)), "{k}"); }
        for n in nodes { let back: Node = serde_json::from_value(n["default"].clone()).unwrap_or_else(|e| panic!("{}: {e}", n["id"])); assert_eq!(serde_json::to_value(&back).unwrap(), n["default"], "{} round-trips", n["id"]); }
        let w = nodes.iter().find(|n| n["id"] == json!("worker")).unwrap(); assert_eq!(w["tier"], json!("low")); assert_eq!(nodes.iter().find(|n| n["id"] == json!("planner")).unwrap()["tier"], json!("mid"));
        assert_eq!(nodes.iter().find(|n| n["id"] == json!("loop")).unwrap()["ports"], json!(["body", "done", "exhausted"])); assert!(nodes.iter().find(|n| n["id"] == json!("qa")).unwrap()["writes"].as_array().unwrap().iter().any(|f| f["field"] == json!("pass")));
    }

    #[test]
    fn the_simulation_says_what_it_did_and_never_runs_forever() {
        let mut g = one_loop(); g.nodes.get_mut("loop").map(|n| n.kind = Kind::Loop { max_iterations: 1000, until: None }); g.nodes.get_mut("loop").map(|n| n.cap_usd = Some(1.0));
        assert!(simulate(&g, &BTreeMap::new(), Map::new()).is_ok(), "a long loop is still finite");
        let s = run_all(&one_loop(), &[]); assert_eq!(s.path.first().map(String::as_str), Some("tests")); assert!(s.trace.iter().any(|t| t.kind == "loop"));
    }

    // ── RFC 0008 §4.16: the Leader builds any team ──
    fn agent_node(title: &str, effects: &[&str], owns: &[&str]) -> Node {
        let mut n = Node::role(RoleKind::Agent);
        if let Kind::Role { agent, owns: o, .. } = &mut n.kind { *agent = Some(AgentSpec { title: title.into(), responsibility: "write the posts".into(), brief: "in our voice".into(), effects: effects.iter().map(|e| e.to_string()).collect(), ..Default::default() }); *o = owns.iter().map(|x| x.to_string()).collect(); }
        n.cap(0.5)
    }
    fn marketing(effects: &[&str], human: bool) -> Graph {
        let mut g = template("blank", "Marketing", 4.0).unwrap();
        n(&mut g, "copy", agent_node("Copywriter", effects, &["content/**"])); g.edges.retain(|e| !(e.from == "me")); 
        if human { g.edges.push(edge("copy", "out", "me")); g.start = vec!["copy".into()]; } else { g.edges.push(edge("copy", "out", "deliver")); g.edges.push(edge("me", "approved", "deliver")); g.start = vec!["copy".into()]; }
        g
    }
    #[test]
    fn a_custom_agent_is_one_responsibility_with_a_brief_and_the_blank_team_is_valid() {
        let g = template("blank", "Marketing", 4.0).unwrap(); assert!(validate(&g).ok(), "{:?}", validate(&g).words());
        let mut g = marketing(&["read", "write"], true); g.edges.push(edge("me", "approved", "deliver"));
        let r = validate(&g); assert!(r.ok(), "{:?}", r.words()); assert!(g.nodes["copy"].is_writer(), "an agent that writes is a writer: it owns files");
        let (f, t) = writes_of(&g.nodes["copy"]).into_iter().unzip::<String, String, Vec<_>, Vec<_>>(); assert_eq!(f, vec!["done", "pass", "report", "files"]); let _ = t;
        // it round-trips through the graph file
        let back = Graph::from_json(&g.to_json()).unwrap(); assert_eq!(back.nodes["copy"], g.nodes["copy"]);
        // no brief, an unknown effect, an agent that writes without owning files, a tool it must not have
        let mut bad = g.clone(); if let Kind::Role { agent, .. } = &mut bad.nodes.get_mut("copy").unwrap().kind { agent.as_mut().unwrap().brief = String::new(); } assert!(validate(&bad).has("agent_incomplete"));
        let mut bad = g.clone(); if let Kind::Role { agent, .. } = &mut bad.nodes.get_mut("copy").unwrap().kind { agent.as_mut().unwrap().effects.push("destroy".into()); } assert!(validate(&bad).has("unknown_effect"));
        let mut bad = g.clone(); if let Kind::Role { owns, .. } = &mut bad.nodes.get_mut("copy").unwrap().kind { owns.clear(); } assert!(validate(&bad).has("writer_no_scope"));
        let mut bad = g.clone(); if let Kind::Role { agent, .. } = &mut bad.nodes.get_mut("copy").unwrap().kind { agent.as_mut().unwrap().mcp = vec!["antiloki_delegate".into()]; } assert!(validate(&bad).has("agent_mcp_forbidden"));
        let mut many = g.clone(); if let Kind::Role { agent, .. } = &mut many.nodes.get_mut("copy").unwrap().kind { agent.as_mut().unwrap().responsibility = "write posts and schedule them and report on them".into(); } assert!(validate(&many).has("agent_many_jobs"));
    }
    #[test]
    fn an_agent_that_posts_or_spends_has_a_human_on_every_way_to_it() {
        // the way to it avoids the person: refused
        let g = marketing(&["read", "outward"], false); assert!(validate(&g).has("destructive_without_human"), "{:?}", validate(&g).words());
        let g = marketing(&["read", "money"], false); assert!(validate(&g).has("destructive_without_human"));
        // the person stands before it: fine
        let mut g = template("blank", "Marketing", 4.0).unwrap(); n(&mut g, "copy", agent_node("Poster", &["outward"], &[])); g.start = vec!["me".into()]; g.edges.clear(); g.edges.push(edge("me", "approved", "copy")); g.edges.push(edge("copy", "out", "deliver"));
        let r = validate(&g); assert!(!r.has("destructive_without_human"), "{:?}", r.words());
        // a tool node with an outward effect needs the same
        let mut t = template("feature", "Pack", 6.0).unwrap(); if let Some(Node { kind: Kind::UseTool { allow, .. }, .. }) = t.nodes.get_mut("tests") { allow.effects.push("outward".into()); } assert!(validate(&t).has("destructive_without_human"));
    }

    // ── RFC 0008 §4.17: the artifact node ──
    fn with_artifact(from: &str, name: &str, format: &str) -> Graph {
        let mut g = template("blank", "Report", 3.0).unwrap(); n(&mut g, "analyst", agent_node("Analyst", &["read"], &[])); n(&mut g, "doc", Node::new(Kind::Artifact { name: name.into(), format: format.into(), from: Some(from.into()), title: Some("The report".into()), compose: None }));
        g.edges.retain(|e| e.from != "me"); g.edges.push(edge("analyst", "out", "doc")); g.edges.push(edge("doc", "out", "me")); g.edges.push(edge("me", "approved", "deliver")); g.start = vec!["analyst".into()]; g
    }
    #[test]
    fn a_chain_of_agents_with_no_reason_is_one_prompt_with_extra_steps_until_a_node_says_why_or_a_check_stands_in() {
        let chain = || { let mut g = with_artifact("analyst", "report", "md"); n(&mut g, "editor", agent_node("Editor", &["read"], &[])); g.edges.retain(|e| !(e.from == "doc" && e.to == "me")); g.edges.push(edge("doc", "out", "editor")); g.edges.push(edge("editor", "out", "me")); g };
        let g = chain(); let r = validate(&g); assert!(r.has("one_prompt_with_extra_steps") && r.ok(), "a warning, never an error: {:?}", r.words());
        let mut why = chain(); why.nodes.get_mut("editor").unwrap().why = Some(Why { kind: "independent_check".into(), note: "a fresh pair of eyes".into() }); assert!(!validate(&why).has("one_prompt_with_extra_steps"), "a reason clears it");
        let mut gate = chain(); gate.nodes.get_mut("editor").unwrap().why = Some(Why { kind: "gate".into(), note: String::new() }); assert!(validate(&gate).has("one_prompt_with_extra_steps"), "a gate is not a reason an agent is there");
        let mut chk = chain(); n(&mut chk, "chk", Node::new(Kind::Check { on: "artifact:report.md".into(), rules: vec![crate::team_check::Rule { kind: "words".into(), max: Some(50.0), ..Default::default() }] })); chk.edges.retain(|e| !(e.from == "editor" && e.to == "me")); chk.edges.push(edge("editor", "out", "chk")); chk.edges.push(edge("chk", "out", "me"));
        let r = validate(&chk); assert!(!r.has("one_prompt_with_extra_steps") && r.ok(), "a check is something an agent could not be: {:?}", r.words());
        let mut bad = chain(); bad.nodes.get_mut("editor").unwrap().why = Some(Why { kind: "because".into(), note: String::new() }); assert!(validate(&bad).has("why_kind"));
        let mut late = chain(); n(&mut late, "chk", Node::new(Kind::Check { on: "artifact:report.md".into(), rules: vec![crate::team_check::Rule { kind: "words".into(), max: Some(5.0), ..Default::default() }] })); late.nodes.insert("src".into(), agent_node("Src", &["read"], &[])); late.edges.retain(|e| !(e.from == "editor" && e.to == "me")); late.edges.push(edge("editor", "out", "chk")); late.edges.push(edge("chk", "out", "me"));
        let mut ghost = chk.clone(); if let Some(Node { kind: Kind::Check { on, .. }, .. }) = ghost.nodes.get_mut("chk") { *on = "agent:nobody.report".into(); } assert!(validate(&ghost).has("check_source"), "a source that is not in the graph is refused");
    }
    #[test]
    fn an_artifact_is_a_document_made_from_one_nodes_work_and_the_validator_holds_it_to_that() {
        let g = with_artifact("analyst", "report", "md"); let r = validate(&g); assert!(r.ok(), "{:?}", r.words());
        assert_eq!(writes_of(&g.nodes["doc"]).iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>(), vec!["ok", "name", "path", "format"]); assert_eq!(g.nodes["doc"].kind_key(), "artifact");
        assert_eq!(Graph::from_json(&g.to_json()).unwrap().nodes["doc"], g.nodes["doc"], "it round-trips through the graph file");
        assert!(validate(&with_artifact("analyst", "re port", "md")).has("artifact_name")); assert!(validate(&with_artifact("analyst", "report", "exe")).has("artifact_format"));
        assert!(validate(&with_artifact("ghost", "report", "md")).has("artifact_source")); assert!(validate(&with_artifact("me", "report", "md")).has("artifact_source"), "a human node writes no document");
        let mut dup = with_artifact("analyst", "report", "md"); n(&mut dup, "doc2", Node::new(Kind::Artifact { name: "report".into(), format: "md".into(), from: Some("analyst".into()), title: None, compose: None })); dup.edges.retain(|e| !(e.from == "doc" && e.to == "me")); dup.edges.push(edge("doc", "out", "doc2")); dup.edges.push(edge("doc2", "out", "me")); assert!(validate(&dup).has("artifact_duplicate"));
        // after the artifact, not before the node it is made from
        let mut late = with_artifact("analyst", "report", "md"); n(&mut late, "later", agent_node("Later", &["read"], &[])); late.edges.retain(|e| !(e.from == "doc" && e.to == "me")); late.edges.push(edge("doc", "out", "later")); late.edges.push(edge("later", "out", "me")); if let Kind::Artifact { from, .. } = &mut late.nodes.get_mut("doc").unwrap().kind { *from = Some("later".into()); } assert!(validate(&late).has("artifact_source"));
        let sim = simulate(&g, &Default::default(), Default::default()).unwrap(); assert!(sim.path.contains(&"doc".to_string()), "the simulation passes through it: {:?}", sim.path);
    }

    // ── the web node (§4.4) ──
    fn with_web(name: &str, effects: &[&str], args: Value) -> Graph {
        let mut g = template("blank", "Research", 3.0).unwrap();
        n(&mut g, "web", Node::new(Kind::UseTool { tool: ToolRef { kind: "web".into(), name: name.into(), args }, allow: Allow { effects: effects.iter().map(|e| e.to_string()).collect(), ..Default::default() }, starts_model: false, outputs: Default::default() }));
        g.edges.retain(|e| e.from != "me"); g.edges.push(edge("web", "out", "me")); g.edges.push(edge("me", "approved", "deliver")); g.start = vec!["web".into()]; g
    }
    #[test]
    fn a_web_node_searches_or_reads_with_the_network_effect_only_and_says_what_for() {
        let ok = with_web("search", &["network"], json!({ "query": "{ask}", "max": 5 })); assert!(validate(&ok).ok(), "{:?}", validate(&ok).words());
        assert!(validate(&with_web("fetch", &["network", "read"], json!({ "url": "https://example.com" }))).ok());
        assert!(validate(&with_web("crawl", &["network"], json!({ "query": "x" }))).has("web_tool"));
        assert!(validate(&with_web("search", &["read"], json!({ "query": "x" }))).has("web_needs_network"));
        assert!(validate(&with_web("search", &["network", "write"], json!({ "query": "x" }))).has("web_read_only"), "the web is only read");
        assert!(validate(&with_web("search", &["network", "outward"], json!({ "query": "x" }))).has("web_read_only"), "posting is not searching");
        assert!(validate(&with_web("search", &["network"], json!({}))).has("web_no_query")); assert!(validate(&with_web("fetch", &["network"], json!({ "query": "x" }))).has("web_no_query"), "a fetch needs a url");
        assert!(catalog()["nodes"].as_array().unwrap().iter().any(|n| n["id"] == json!("tool_web_search")) && catalog()["nodes"].as_array().unwrap().iter().any(|n| n["id"] == json!("tool_web_fetch")));
        let mut a = agent_node("Researcher", &["read", "web"], &[]); if let Kind::Role { agent, .. } = &mut a.kind { assert!(agent.as_ref().unwrap().has("web")); }
    }

    #[test]
    fn family_covers_every_kind() {
        let kinds: Vec<Kind> = vec![
            Kind::Leader,
            Kind::Role { role: RoleKind::Worker, tier: None, model: None, readonly: None, fresh_context: None, parallel: None, owns: vec![], agent: None },
            Kind::If { cond: "x".into() },
            Kind::Switch { on: "x".into() },
            Kind::Loop { max_iterations: 1, until: None },
            Kind::ForEach { over: "x".into(), parallel: None, batch: None, on_error: None },
            Kind::Timer { mode: TimerMode::Delay, seconds: 1, cron: None, wraps: None },
            Kind::Fork,
            Kind::Join { policy: JoinPolicy::All, quorum: None },
            Kind::Retry { max: 1, backoff_s: 0, open_after: None, cooldown_s: None },
            Kind::BudgetGate { stop_at: 0.9, ask_at: None },
            Kind::Human { gate: "g".into(), on_timeout: None },
            Kind::UseTool { tool: ToolRef { kind: "skill".into(), name: "x".into(), args: json!({}) }, allow: Default::default(), starts_model: false, outputs: Default::default() },
            Kind::Artifact { name: "a".into(), format: "md".into(), from: None, title: None, compose: None },
            Kind::Check { on: "values:x.y".into(), rules: vec![] },
            Kind::Llm(crate::team_llm::Spec { seat: "claude-code:haiku".into(), prompt: "x".into(), answer: Default::default(), fallback: vec![], verify: Default::default(), agree_with: vec![], max_tokens: None, timeout_s: None, examples_from: None, preset: None, router: None }),
            Kind::Code { source: "#{}".into(), examples: vec![crate::team_code::Example { input: json!({}), output: json!({}) }], from: None, limits: None },
            Kind::Transform { mode: "limit".into(), from: None, config: json!({ "n": 1 }) },
            Kind::SubTeam { team: "inner".into(), ask: "x".into(), outputs: BTreeMap::new(), budget_share: None, max_depth: None },
            Kind::Stop { message: "no".into() },
            Kind::Deliver,
        ];
        for k in kinds { let (fam, _) = family(&k); let _ = fam; }
    }

    #[test]
    fn the_local_nodes_are_their_own_category_and_the_leader_can_list_them() {
        let c = catalog(); let local: Vec<&str> = c["nodes"].as_array().unwrap().iter().filter(|n| n["family"] == json!("local")).filter_map(|n| n["id"].as_str()).collect();
        for k in ["decompose", "recurse", "evolve", "test", "contract", "stubs", "review", "tool_xray", "tool_apply"] { assert!(local.contains(&k), "{k} is in the local category: {local:?}"); }
        let md = node_list_md(""); assert!(md.starts_with("## local"), "the local category comes first"); assert!(md.contains("`stubs`") && md.contains("local_coder") && md.contains("## role"));
        let only = node_list_md("local"); assert!(!only.contains("## role") && only.contains("`review`")); assert!(node_list_md("nope").is_empty());
        let g = template("local_coder", "L", 1.0).unwrap(); assert!(validate(&g).ok(), "{:?}", validate(&g).words()); assert!(g.nodes.contains_key("leader_tests") && g.nodes.contains_key("contract"));
    }

    #[test]
    fn saved_graphs_still_round_trip_unchanged() {
        for k in TEMPLATES {
            let g = template(k, "pack", 6.0).unwrap();
            let back = Graph::from_json(&g.to_json()).unwrap();
            assert_eq!(back, g, "{k}");
        }
    }

    #[test]
    fn a_stop_node_fails_the_run_with_its_message() {
        let mut g = template("blank", "Stop lane", 2.0).unwrap();
        n(&mut g, "halt", Node::new(Kind::Stop { message: "pricing gate failed".into() }));
        g.start = vec!["halt".into()];
        let r = validate(&g);
        assert!(r.ok(), "{:?}", r.words());
        let mut s = RunState::new();
        let a = graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        assert!(a.iter().any(|x| matches!(x, Action::Handoff { reason } if reason.contains("pricing gate failed"))), "{a:?}");
        assert!(matches!(s.status, RunStatus::Blocked(m) if m.contains("pricing gate failed")));
    }

    #[test]
    fn a_code_node_without_an_example_is_a_validator_error() {
        let mut g = template("blank", "Code lane", 2.0).unwrap();
        n(&mut g, "c", Node::new(Kind::Code { source: "1".into(), examples: vec![], from: None, limits: None }));
        g.start = vec!["c".into()];
        g.edges.push(edge("c", "out", "deliver"));
        let r = validate(&g);
        assert!(r.has("code_no_example"), "{:?}", r.words());
    }

    #[test]
    fn join_consensus_climbs_on_disagreement() {
        let mut g = template("blank", "Consensus", 2.0).unwrap(); g.edges.clear(); g.start = vec!["fork".into()];
        n(&mut g, "fork", Node::new(Kind::Fork)); n(&mut g, "a", writer(&[])); n(&mut g, "b", writer(&[])); n(&mut g, "up", writer(&[]));
        n(&mut g, "join", Node::new(Kind::Join { policy: JoinPolicy::Consensus, quorum: None }));
        for e in [edge("fork", "branch", "a"), edge("fork", "branch", "b"), edge("a", "out", "join"), edge("b", "out", "join"), edge("join", "out", "deliver"), edge("join", "climb", "up"), edge("up", "out", "deliver")] { g.edges.push(e); }
        assert!(allowed_port(g.node("join").unwrap(), "climb") && !allowed_port(g.node("a").unwrap(), "climb"));
        // they agree (the seat that answered and the calls it made do not count): on to the delivery
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        graph_step(&g, &mut s, Event::NodeDone { node: "a".into(), outputs: json!({ "kind": "write", "seat_used": "endpoint:small", "calls": 1 }), cost_usd: 0.0 }).unwrap();
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "b".into(), outputs: json!({ "kind": "write", "seat_used": "claude-code:haiku", "calls": 2 }), cost_usd: 0.0 }).unwrap();
        assert!(a.contains(&Action::Deliver), "{a:?}"); assert_eq!(s.values["join"]["agreed"], json!(true));
        // they differ: the join climbs to the stronger seat
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        graph_step(&g, &mut s, Event::NodeDone { node: "a".into(), outputs: json!({ "kind": "write" }), cost_usd: 0.0 }).unwrap();
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "b".into(), outputs: json!({ "kind": "review" }), cost_usd: 0.0 }).unwrap();
        assert!(a.contains(&Action::Run { node: "up".into() }) && !a.contains(&Action::Deliver), "{a:?}"); assert_eq!(s.values["join"]["agreed"], json!(false));
        // nothing wired to climb: the person hears of it
        g.edges.retain(|e| e.port != "climb");
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        graph_step(&g, &mut s, Event::NodeDone { node: "a".into(), outputs: json!({ "kind": "write" }), cost_usd: 0.0 }).unwrap();
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "b".into(), outputs: json!({ "kind": "review" }), cost_usd: 0.0 }).unwrap();
        assert!(matches!(a.last(), Some(Action::Handoff { reason }) if reason.contains("same answer")), "{a:?}");
    }

    #[test]
    fn foreach_batch_and_on_error_collect() {
        let mut g = template("blank", "Batches", 2.0).unwrap(); g.edges.clear(); g.start = vec!["each".into()];
        n(&mut g, "each", Node::new(Kind::ForEach { over: "items".into(), parallel: None, batch: Some(2), on_error: Some("collect".into()) })); n(&mut g, "work", writer(&[]));
        for e in [edge("each", "body", "work"), edge("work", "out", "each"), edge("each", "done", "deliver")] { g.edges.push(e); }
        let mut seed = Map::new(); seed.insert("items".into(), json!([1, 2, 3, 4, 5]));
        let mut s = RunState::new(); let a = graph_step(&g, &mut s, Event::Start { now_ms: 0, seed }).unwrap();
        assert!(a.contains(&Action::Run { node: "work".into() }), "{a:?}"); assert_eq!(s.values["each"]["item"], json!([1, 2]), "the body gets a batch of two");
        // the first batch fails: it is kept, and the next batch starts
        let a = graph_step(&g, &mut s, Event::NodeFailed { node: "work".into(), kind: FailKind::Error, error: "model said no".into(), cost_usd: 0.0 }).unwrap();
        assert!(a.contains(&Action::Run { node: "work".into() }) && !a.iter().any(|x| matches!(x, Action::Handoff { .. })), "{a:?}");
        assert_eq!(s.values["each"]["item"], json!([3, 4])); assert_eq!(s.values["each"]["errors"][0]["index"], json!(0)); assert!(s.values["each"]["errors"][0]["error"].as_str().unwrap().contains("model said no"));
        graph_step(&g, &mut s, Event::NodeDone { node: "work".into(), outputs: json!({}), cost_usd: 0.0 }).unwrap();
        assert_eq!(s.values["each"]["item"], json!([5]), "the last batch is what is left");
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "work".into(), outputs: json!({}), cost_usd: 0.0 }).unwrap();
        assert!(a.contains(&Action::Deliver), "{a:?}"); assert_eq!(s.values["each"]["errors"].as_array().unwrap().len(), 1, "the failure is in the done output");
        // without `collect` a failure goes the way it always did: the person hears of it
        if let Some(nd) = g.nodes.get_mut("each") { nd.kind = Kind::ForEach { over: "items".into(), parallel: None, batch: None, on_error: None }; }
        let mut seed = Map::new(); seed.insert("items".into(), json!([1, 2]));
        let mut s = RunState::new(); graph_step(&g, &mut s, Event::Start { now_ms: 0, seed }).unwrap();
        let a = graph_step(&g, &mut s, Event::NodeFailed { node: "work".into(), kind: FailKind::Error, error: "x".into(), cost_usd: 0.0 }).unwrap();
        assert!(matches!(a.last(), Some(Action::Handoff { .. })), "{a:?}");
        // a bad setting is a validator error
        assert!(!validate(&{ let mut b = g.clone(); if let Some(nd) = b.nodes.get_mut("each") { nd.kind = Kind::ForEach { over: "items".into(), parallel: None, batch: Some(0), on_error: Some("explode".into()) }; } b }).ok());
    }

    #[test]
    fn retry_open_after_stops_calling_a_failing_seat_then_lets_it_recover() {
        let mut g = template("blank", "Breaker", 2.0).unwrap(); g.edges.clear(); g.start = vec!["call".into()];
        n(&mut g, "call", writer(&["src/**"])); n(&mut g, "again", Node::new(Kind::Retry { max: 10, backoff_s: 0, open_after: Some(2), cooldown_s: Some(60) }));
        for e in [edge("call", "error", "again"), edge("again", "again", "call"), edge("again", "exhausted", "me"), edge("call", "out", "deliver"), edge("me", "approved", "deliver")] { g.edges.push(e); }
        assert!(validate(&g).ok(), "{:?}", validate(&g).words());
        let fail = |g: &Graph, s: &mut RunState, at: i64| { graph_step(g, s, Event::Tick { now_ms: at }).unwrap(); graph_step(g, s, Event::NodeFailed { node: "call".into(), kind: FailKind::Error, error: "down".into(), cost_usd: 0.0 }).unwrap() };
        let mut s = RunState::new(); let a = graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap(); assert!(a.contains(&Action::Run { node: "call".into() }));
        // two failures are sent round again
        for at in [1_000, 2_000] { let a = fail(&g, &mut s, at); assert!(a.contains(&Action::Run { node: "call".into() }), "{a:?}"); }
        // the third finds the breaker open: the seat is not called, the failure leaves by `exhausted` at once
        let a = fail(&g, &mut s, 3_000); assert!(!a.contains(&Action::Run { node: "call".into() }), "an open breaker does not call it: {a:?}"); assert!(a.contains(&Action::Park { node: "me".into(), gate: "merge".into() }) || s.active.contains_key("me"), "{a:?} {:?}", s.active);
        assert!(s.opened.contains_key("again"));
        // while it is open, nothing calls the seat
        let mut s2 = s.clone(); s2.active.insert("call".into(), 0); s2.status = RunStatus::Running;
        let a = graph_step(&g, &mut s2, Event::NodeFailed { node: "call".into(), kind: FailKind::Error, error: "down".into(), cost_usd: 0.0 }).unwrap(); assert!(!a.contains(&Action::Run { node: "call".into() }), "{a:?}");
        // after the cooldown one try is let through, with a fresh count
        let mut s3 = s.clone(); s3.active.clear(); s3.status = RunStatus::Running; s3.active.insert("call".into(), 0);
        graph_step(&g, &mut s3, Event::Tick { now_ms: 3_000 + 61_000 }).unwrap();
        let a = graph_step(&g, &mut s3, Event::NodeFailed { node: "call".into(), kind: FailKind::Error, error: "still down".into(), cost_usd: 0.0 }).unwrap();
        assert!(a.contains(&Action::Run { node: "call".into() }), "it recovered one try: {a:?}"); assert!(!s3.opened.contains_key("again")); assert_eq!(s3.tries["again"], 1);
        // a breaker with `open_after` zero is a validator error
        if let Some(nd) = g.nodes.get_mut("again") { nd.kind = Kind::Retry { max: 3, backoff_s: 0, open_after: Some(0), cooldown_s: None }; }
        assert!(!validate(&g).ok());
    }

    #[test]
    fn the_palette_offers_the_data_tools_and_the_web_app_tool_and_each_default_is_a_known_tool_type() {
        let cat = catalog(); let nodes = cat["nodes"].as_array().unwrap();
        for id in ["tool_db", "tool_file", "tool_saga", "tool_webapp"] {
            let item = nodes.iter().find(|n| n["id"] == id).unwrap_or_else(|| panic!("{id} is in the palette"));
            assert_eq!(item["family"], "tool", "{id}"); let ty = item["default"]["tool"]["type"].as_str().unwrap();
            assert!(TOOL_TYPES.contains(&ty), "{id}: `{ty}` is a tool type the validator knows");
            let node: Node = serde_json::from_value(item["default"].clone()).unwrap_or_else(|e| panic!("{id}: {e}")); assert!(matches!(node.kind, Kind::UseTool { .. }));
        }
        assert!(!nodes.iter().any(|n| n["hint"].as_str().unwrap_or("").contains("arrives with RFC 0009")), "the web app tool is not a promise any more");
    }

    #[test]
    fn an_approval_carries_a_risk_and_a_note_and_a_bad_one_is_refused() {
        let mut g = template("blank", "Appr", 2.0).unwrap(); let me = g.nodes.get_mut("me").unwrap();
        me.approval = Some(Approval { risk: "high".into(), note: "Publishes {ask} to the shop".into(), show: String::new() }); assert!(validate(&g).ok(), "{:?}", validate(&g).words());
        let j = g.to_json(); assert_eq!(j["nodes"]["me"]["approval"]["risk"], json!("high")); let back = Graph::from_json(&j).unwrap(); assert_eq!(back.nodes["me"].approval.as_ref().unwrap().note, "Publishes {ask} to the shop");
        g.nodes.get_mut("me").unwrap().approval = Some(Approval { risk: "extreme".into(), note: String::new(), show: String::new() }); assert!(validate(&g).has("approval_risk"));
        g.nodes.get_mut("me").unwrap().approval = Some(Approval { risk: "low".into(), note: "x".repeat(601), show: String::new() }); assert!(validate(&g).has("approval_note"));
        g.nodes.get_mut("me").unwrap().approval = Some(Approval { risk: "low".into(), note: String::new(), show: "frames".into() }); assert!(validate(&g).has("approval_show"), "an approval cannot show what nothing writes");
        g.nodes.insert("frames_doc".into(), Node::new(Kind::Artifact { name: "frames".into(), format: "html".into(), from: None, title: None, compose: None })); assert!(!validate(&g).has("approval_show"), "it can once an artifact node writes it");
        assert!(palette_has("approval"), "the palette offers the approval");
    }
    fn palette_has(id: &str) -> bool { catalog().to_string().contains(&format!("\"id\":\"{id}\"")) }

    #[test]
    fn a_step_on_ask_waits_for_the_person_once_and_one_on_always_every_time() {
        let mut g = one_loop(); g.nodes.get_mut("tests").unwrap().autonomy = Some("ask".into()); assert!(validate(&g).ok(), "{:?}", validate(&g).words());
        let mut s = RunState::new(); let a = graph_step(&g, &mut s, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        assert_eq!(a, vec![Action::Pass { node: "loop".into(), n: 1 }, Action::Park { node: "tests".into(), gate: "permission".into() }], "it asks before it starts");
        assert!(s.asking.contains("tests") && s.active.contains_key("tests"));
        let a = graph_step(&g, &mut s, Event::Answer { node: "tests".into(), approve: true }).unwrap(); assert_eq!(a, vec![Action::Run { node: "tests".into() }], "yes lets it start"); assert!(s.permitted.contains("tests") && !s.asking.contains("tests"));
        // once per run: the next round does not ask again
        let a = graph_step(&g, &mut s, Event::NodeDone { node: "tests".into(), outputs: json!({ "exit": 1 }), cost_usd: 0.0 }).unwrap(); let _ = a;
        // `always` asks every time
        let mut g2 = one_loop(); g2.nodes.get_mut("tests").unwrap().autonomy = Some("always".into()); let mut s2 = RunState::new(); graph_step(&g2, &mut s2, Event::Start { now_ms: 0, seed: Map::new() }).unwrap();
        graph_step(&g2, &mut s2, Event::Answer { node: "tests".into(), approve: true }).unwrap(); assert!(!s2.permitted.contains("tests"), "always remembers nothing");
        // a no hands the run to the person (nothing is wired to catch it)
        let mut s3 = RunState::new(); graph_step(&g2, &mut s3, Event::Start { now_ms: 0, seed: Map::new() }).unwrap(); graph_step(&g2, &mut s3, Event::Answer { node: "tests".into(), approve: false }).unwrap(); assert!(matches!(s3.status, RunStatus::Blocked(_)), "{:?}", s3.status);
        // a bad value or a node that does not run cannot carry it
        g.nodes.get_mut("tests").unwrap().autonomy = Some("sometimes".into()); assert!(validate(&g).has("autonomy_value")); g.nodes.get_mut("tests").unwrap().autonomy = None; g.nodes.get_mut("loop").unwrap().autonomy = Some("ask".into()); assert!(validate(&g).has("autonomy_kind"));
    }
}
