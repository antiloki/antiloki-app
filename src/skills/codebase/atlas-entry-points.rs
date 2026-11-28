//! atlas-entry-points.rs — twin of skills/codebase/atlas-entry-points.mjs. NAMES ARE CONTRACT:
//! `detectEntryPoints` (named, no-db, pure) · `atlasEntryPoints` (default, standard db+fs lane) ·
//! `writeEntryPoints` (named, db-first helper).
//!
//! `atlasEntryPoints` reads `{root}/{code_nodes.path}` off the REAL filesystem for every atlas
//! file row — same fixture-controllable shape as `stack-facts.rs`/`code-signals-detect.rs`: `root`
//! is DB-derived (`input.root ?? getWorkspaceRoot(db)`, an already-converted twin), so the fixture
//! seeds an active `workspaces` row pointing at a real on-disk testdata dir
//! (`antiloki-crossing/fixtures/testdata/atlas-entry-points-basic/`) committed alongside this
//! unit's fixtures. A file whose row has no matching on-disk file hits the JS `catch { continue;
//! }` — modeled as `Err(_) => continue`.
//!
//! `context.db` missing → JS throws `err.contextMissing(...)`. Not modeled: `db: &Connection` is a
//! required, non-nullable reference here — same "guard-throws are not exercised" convention used
//! by every sibling twin. `context.onProgress` (+ the `setImmediate` event-loop yield) is a pure
//! side effect with zero influence on the return value or `db_after` — not modeled at all.
//!
//! Each matcher's row is its own JS object literal, and JS object literals OMIT any key never
//! written — there is no `key: undefined` in the wire shape, the key is simply absent. The
//! matchers fall into exactly three literal shapes: (1) the default
//! `{ kind, subtype, framework, label, confidence, line }` used by everything except (2) and (3);
//! (2) `routesNest`/`routesExpress`, which insert `http_method` before `label`:
//! `{ kind, subtype, framework, http_method, label, confidence, line }`; (3) `dataSqlite`/
//! `outboundCalls`, which insert `detail` after `label`:
//! `{ kind, subtype, framework, label, detail, confidence, line }`. No matcher ever writes both
//! `http_method` and `detail`. `detectEntryPoints` is itself a directly fixture-tested unit (its
//! raw row array is compared verbatim), so the exact per-matcher key SET matters, not just what
//! `writeEntryPoints` later reads off each row — `ep`/`ep_route`/`ep_detail` below build the three
//! shapes explicitly (in JS insertion order, even though the JS-vs-Rust comparator sorts keys
//! before comparing, so a stricter future comparator stays correct too) instead of one canonical
//! always-present-null helper.
//!
//! `code_entry_points.created_at` / `code_entry_point_snapshots.captured_at` are
//! `DEFAULT (datetime('now'))` columns the skill's own INSERT never sets — the conform-db JS lane
//! only freezes SQL text the SKILL executes (`freezeDb` wraps `context.db`), not the fixture's own
//! raw `seedDb` schema exec, so a table DEFAULT expression evaluates the REAL wall clock in both
//! lanes. This unit's fixture therefore pins those DEFAULTs to the frozen literal
//! (`DEFAULT '2026-01-01 00:00:00'`) directly in its `schema` DDL instead of `datetime('now')` —
//! same instant either way, but deterministic across both lanes and repeat runs.
// QUALITY: 78/100 — 12 regex-matcher families ported 1:1 (no lookaround needed); write lane keeps
// SAVEPOINT/rollback shape; snapshot diff (`Set` insertion order) modeled with an order-preserving
// Vec+HashSet pair since Rust's HashSet has no stable iteration order.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// `const lineOf = (src, idx) => { let n = 1; for (let i = 0; i < idx && i < src.length; i++) if
// (src[i] === '\n') n++; return n; };` — fixtures are ASCII, so a byte-index walk is exact.
fn line_of(src: &str, idx: usize) -> i64 {
    let bound = idx.min(src.len());
    src.as_bytes()[..bound].iter().filter(|&&b| b == b'\n').count() as i64 + 1
}
// `const clip = (s, n = 140) => String(s ?? '').replace(/\s+/g, ' ').trim().slice(0, n);`
fn ws_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s+").unwrap()) }
fn clip(s: &str, n: usize) -> String { ws_re().replace_all(s, " ").trim().chars().take(n).collect() }

// ── SQL verb + table → {subtype, label} ─────────────────────────────────────────────────
fn verb_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)^\s*(SELECT|INSERT|UPDATE|DELETE|REPLACE|CREATE|DROP|ALTER)\b").unwrap()) }
fn from_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bFROM\s+([A-Za-z_][\w.]*)").unwrap()) }
fn into_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bINTO\s+([A-Za-z_][\w.]*)").unwrap()) }
fn update_tbl_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\bUPDATE\s+([A-Za-z_][\w.]*)").unwrap()) }
fn table_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\b(?:TABLE(?:\s+IF\s+NOT\s+EXISTS)?)\s+([A-Za-z_][\w.]*)").unwrap()) }
fn sql_entry(sql: &str) -> Option<(String, String)> {
    let s = sql.trim();
    let verb = verb_re().captures(s).and_then(|c| c.get(1)).map(|m| m.as_str().to_uppercase());
    let verb = match verb { Some(v) if !v.is_empty() => v, _ => return None };
    let table = from_re().captures(s).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
        .or_else(|| into_re().captures(s).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()))
        .or_else(|| update_tbl_re().captures(s).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()))
        .or_else(|| table_re().captures(s).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()));
    let subtype = if verb == "SELECT" { "db-read" } else { "db-write" };
    let label = match table { Some(t) => format!("{verb} {t}"), None => verb };
    Some((subtype.to_string(), label))
}

// Default row shape — see module doc: `{ kind, subtype, framework, label, confidence, line }`,
// no `http_method`/`detail` key at all (matches every matcher except the two below).
fn ep(kind: &str, subtype: &str, framework: &str, label: &str, confidence: f64, line: i64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("kind".into(), json!(kind));
    m.insert("subtype".into(), json!(subtype));
    m.insert("framework".into(), json!(framework));
    m.insert("label".into(), json!(label));
    m.insert("confidence".into(), json!(confidence));
    m.insert("line".into(), json!(line));
    m
}

// `routesNest`/`routesExpress` shape: `http_method` is written before `label` in the JS literal.
fn ep_route(kind: &str, subtype: &str, framework: &str, http_method: &str, label: &str, confidence: f64, line: i64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("kind".into(), json!(kind));
    m.insert("subtype".into(), json!(subtype));
    m.insert("framework".into(), json!(framework));
    m.insert("http_method".into(), json!(http_method));
    m.insert("label".into(), json!(label));
    m.insert("confidence".into(), json!(confidence));
    m.insert("line".into(), json!(line));
    m
}

// `dataSqlite`/`outboundCalls` shape: `detail` is written right after `label` in the JS literal.
fn ep_detail(kind: &str, subtype: &str, framework: &str, label: &str, detail: &str, confidence: f64, line: i64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("kind".into(), json!(kind));
    m.insert("subtype".into(), json!(subtype));
    m.insert("framework".into(), json!(framework));
    m.insert("label".into(), json!(label));
    m.insert("detail".into(), json!(detail));
    m.insert("confidence".into(), json!(confidence));
    m.insert("line".into(), json!(line));
    m
}

// ── routes: NestJS / Express / TanStack ─────────────────────────────────────────────────
fn controller_prefix_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"@Controller\(\s*['"`]([^'"`]*)['"`]"#).unwrap()) }
fn strip_slashes_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"^/|/$").unwrap()) }
fn nest_route_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"@(Get|Post|Put|Delete|Patch|All)\(\s*(?:['"`]([^'"`]*)['"`])?\s*\)"#).unwrap()) }
fn routes_nest(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let prefix_raw = controller_prefix_re().captures(source).and_then(|c| c.get(1)).map(|m| m.as_str()).unwrap_or("");
    let prefix = strip_slashes_re().replace_all(prefix_raw, "").into_owned();
    for caps in nest_route_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let method = caps.get(1).unwrap().as_str();
        let seg_raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let seg = seg_raw.strip_prefix('/').unwrap_or(seg_raw);
        let mut parts: Vec<&str> = Vec::new();
        if !prefix.is_empty() { parts.push(prefix.as_str()); }
        if !seg.is_empty() { parts.push(seg); }
        let label = format!("/{}", parts.join("/"));
        let http_method = method.to_uppercase();
        let m = ep_route("route", "http-route", "nestjs", &http_method, &label, 0.92, line_of(source, m0.start()));
        out.push(Value::Object(m));
    }
    out
}
fn express_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\b(?:app|router)\.(get|post|put|delete|patch|all)\(\s*['"`]([^'"`]+)['"`]"#).unwrap()) }
fn routes_express(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in express_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let method = caps.get(1).unwrap().as_str();
        let label = caps.get(2).unwrap().as_str();
        let http_method = method.to_uppercase();
        let m = ep_route("route", "http-route", "express", &http_method, label, 0.82, line_of(source, m0.start()));
        out.push(Value::Object(m));
    }
    out
}
fn tanstack_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"create(?:File)?Route\(\s*['"`]([^'"`]+)['"`]"#).unwrap()) }
fn routes_tanstack(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in tanstack_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let label = caps.get(1).unwrap().as_str();
        out.push(Value::Object(ep("route", "page-route", "tanstack", label, 0.85, line_of(source, m0.start()))));
    }
    out
}

// ── data: node:sqlite / better-sqlite .prepare()/.exec() ───────────────────────────────
fn sql_call_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\.(?:prepare|exec)\s*\(\s*[`'"]([\s\S]*?)[`'"]\s*[),]"#).unwrap()) }
fn data_sqlite(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in sql_call_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let sql = caps.get(1).unwrap().as_str();
        if let Some((subtype, label)) = sql_entry(sql) {
            let detail = clip(sql, 140);
            let m = ep_detail("data", &subtype, "sqlite", &label, &detail, 0.8, line_of(source, m0.start()));
            out.push(Value::Object(m));
        }
    }
    out
}

// ── ui: React handlers + form elements ──────────────────────────────────────────────────
fn ui_handler_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bon(Click|Submit|Change)\s*=").unwrap()) }
fn ui_element_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"<(input|form|select|textarea)\b([^>]*)>").unwrap()) }
fn ui_name_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\b(?:name|placeholder|id)\s*=\s*['"]([^'"]+)['"]"#).unwrap()) }
fn ui_react(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in ui_handler_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let g1 = caps.get(1).unwrap().as_str(); // Click|Submit|Change
        let k = g1.to_lowercase();
        let subtype = if k == "submit" { "form-submit" } else if k == "change" { "form-field" } else { "click" };
        out.push(Value::Object(ep("ui", subtype, "react", &format!("on{g1}"), 0.62, line_of(source, m0.start()))));
    }
    for caps in ui_element_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let tag = caps.get(1).unwrap().as_str();
        let attrs = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let name = ui_name_re().captures(attrs).and_then(|c| c.get(1)).map(|m| m.as_str());
        let label = match name { Some(n) => format!("<{tag}> {n}"), None => format!("<{tag}>") };
        out.push(Value::Object(ep("ui", "form-field", "react", &label, 0.6, line_of(source, m0.start()))));
    }
    out
}

// ── inbound: WebSocket / scheduled / queue / graphql / cli ─────────────────────────────
fn ws_gateway_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"@WebSocketGateway\s*\(").unwrap()) }
fn ws_message_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"@SubscribeMessage\(\s*['"`]([^'"`]+)['"`]"#).unwrap()) }
fn ws_raw_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\.on\(\s*['"`]connection['"`]"#).unwrap()) }
fn ws_inbound(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for m in ws_gateway_re().find_iter(source) {
        out.push(Value::Object(ep("route", "ws-gateway", "nestjs", "WebSocketGateway", 0.9, line_of(source, m.start()))));
    }
    for caps in ws_message_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let label = caps.get(1).unwrap().as_str();
        out.push(Value::Object(ep("route", "ws-message", "nestjs", label, 0.9, line_of(source, m0.start()))));
    }
    for m in ws_raw_re().find_iter(source) {
        out.push(Value::Object(ep("route", "ws-gateway", "ws", "on connection", 0.6, line_of(source, m.start()))));
    }
    out
}
fn cron_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"@Cron\(\s*(?:['"`]([^'"`]+)['"`]|([A-Za-z_$][\w$.]*))"#).unwrap()) }
fn node_cron_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\bcron\.schedule\(\s*['"`]([^'"`]+)['"`]"#).unwrap()) }
fn scheduled(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in cron_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let label = caps.get(1).map(|m| m.as_str()).or_else(|| caps.get(2).map(|m| m.as_str())).unwrap_or("cron");
        out.push(Value::Object(ep("route", "schedule", "nestjs", label, 0.9, line_of(source, m0.start()))));
    }
    for caps in node_cron_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let label = caps.get(1).unwrap().as_str();
        out.push(Value::Object(ep("route", "schedule", "node-cron", label, 0.85, line_of(source, m0.start()))));
    }
    out
}
fn event_pattern_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"@(EventPattern|MessagePattern)\(\s*['"`]([^'"`]+)['"`]"#).unwrap()) }
fn bull_worker_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"new\s+Worker\s*\(\s*['"`]([^'"`]+)['"`]\s*,"#).unwrap()) }
fn queue_consumers(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in event_pattern_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let label = caps.get(2).unwrap().as_str();
        out.push(Value::Object(ep("route", "queue-consume", "nestjs", label, 0.9, line_of(source, m0.start()))));
    }
    for caps in bull_worker_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let label = caps.get(1).unwrap().as_str();
        out.push(Value::Object(ep("route", "queue-consume", "bullmq", label, 0.7, line_of(source, m0.start()))));
    }
    out
}
fn resolver_gate_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"@Resolver\s*\(").unwrap()) }
fn graphql_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"@(Query|Mutation|Subscription)\s*\((?:[^()]|\([^()]*\))*\)\s*(?:async\s+)?([A-Za-z_$][\w$]*)").unwrap()) }
fn graphql_resolvers(source: &str) -> Vec<Value> {
    if !resolver_gate_re().is_match(source) { return Vec::new(); }
    let mut out = Vec::new();
    for caps in graphql_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let verb = caps.get(1).unwrap().as_str().to_lowercase();
        let name = caps.get(2).unwrap().as_str();
        out.push(Value::Object(ep("route", "graphql", "graphql", &format!("{verb} {name}"), 0.88, line_of(source, m0.start()))));
    }
    out
}
fn cli_command_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\.command\(\s*['"`]([^'"`]+)['"`]"#).unwrap()) }
fn cli_commands(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in cli_command_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let label = caps.get(1).unwrap().as_str();
        out.push(Value::Object(ep("route", "cli-command", "cli", label, 0.7, line_of(source, m0.start()))));
    }
    out
}

// ── outbound: fetch / axios / new WebSocket ─────────────────────────────────────────────
fn abs_url_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)^[a-z]+://").unwrap()) }
fn fetch_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\bfetch\s*\(\s*[`'"]([^`'"\n]+)[`'"]"#).unwrap()) }
fn axios_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"\baxios(?:\.(?:get|post|put|delete|patch|head|request))?\s*\(\s*[`'"]([^`'"\n]+)[`'"]"#).unwrap()) }
fn ws_client_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"new\s+WebSocket\s*\(\s*[`'"]([^`'"\n]+)[`'"]"#).unwrap()) }
// `new URL(u).host` — only ever called on strings that already passed the abs_url_re test.
fn host_of(u: &str) -> String {
    if let Some(rest) = u.split_once("://").map(|(_, r)| r) {
        let authority_end = rest.find(|c| c == '/' || c == '?' || c == '#').unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let host = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
        if !host.is_empty() { return host.to_string(); }
    }
    u.chars().take(60).collect()
}
fn outbound_push(out: &mut Vec<Value>, subtype: &str, framework: &str, url: &str, idx: usize, source: &str) {
    let label = if abs_url_re().is_match(url) { host_of(url) } else { url.chars().take(60).collect::<String>() };
    let detail = clip(url, 120);
    let m = ep_detail("outbound", subtype, framework, &label, &detail, 0.8, line_of(source, idx));
    out.push(Value::Object(m));
}
fn outbound_calls(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in fetch_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        outbound_push(&mut out, "http-call", "fetch", caps.get(1).unwrap().as_str(), m0.start(), source);
    }
    for caps in axios_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        outbound_push(&mut out, "http-call", "axios", caps.get(1).unwrap().as_str(), m0.start(), source);
    }
    for caps in ws_client_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        outbound_push(&mut out, "ws-client", "ws", caps.get(1).unwrap().as_str(), m0.start(), source);
    }
    out
}

// ── config: process.env.KEY / import.meta.env.KEY (deduped per file) ───────────────────
fn env_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(?:process\.env|import\.meta\.env)\.([A-Z][A-Z0-9_]{1,63})").unwrap()) }
fn config_reads(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for caps in env_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let key = caps.get(1).unwrap().as_str().to_string();
        if !seen.insert(key.clone()) { continue; }
        let framework = if m0.as_str().starts_with("import") { "vite" } else { "node" };
        out.push(Value::Object(ep("config", "env-read", framework, &key, 0.95, line_of(source, m0.start()))));
    }
    out
}

/// `export function detectEntryPoints(source)` — every matcher over one file's source → the
/// combined entry-point list (no node_id/path yet). Pure, no-db, unit-testable on inline snippets.
pub fn detectEntryPoints(source: &Value) -> Value {
    let s = str_qq(Some(source)); // `String(source ?? '')`
    let mut out: Vec<Value> = Vec::new();
    out.extend(routes_nest(&s));
    out.extend(routes_express(&s));
    out.extend(routes_tanstack(&s));
    out.extend(data_sqlite(&s));
    out.extend(ui_react(&s));
    out.extend(ws_inbound(&s));
    out.extend(scheduled(&s));
    out.extend(queue_consumers(&s));
    out.extend(graphql_resolvers(&s));
    out.extend(cli_commands(&s));
    out.extend(outbound_calls(&s));
    out.extend(config_reads(&s));
    json!(out)
}

/// `export default async function atlasEntryPoints(input, context)` — async-over-sync (playbook's
/// async lane): scan every atlas file's REAL on-disk source, detect, write. `input` is unused
/// beyond `origin`/`root`/`capture`.
pub fn atlasEntryPoints(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let root_v = match nz(input.get("root")) {
        Some(r) => r.clone(),
        None => crate::engine_workspace_root::getWorkspaceRoot(db),
    };
    let root = root_v.as_str().unwrap_or("");

    let files = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin = ? AND type = 'file'", &[origin.clone()])?;
    let mut rows: Vec<Value> = Vec::new();
    for f in files.as_array().cloned().unwrap_or_default() {
        let path_v = f.get("path").cloned().unwrap_or(Value::Null);
        let path_str = path_v.as_str().unwrap_or("");
        let full = std::path::Path::new(root).join(path_str);
        let src = match std::fs::read_to_string(&full) { Ok(s) => s, Err(_) => continue }; // `catch { continue; }`
        if let Some(arr) = detectEntryPoints(&json!(src)).as_array() {
            for e in arr {
                let mut m = e.as_object().cloned().unwrap_or_default();
                m.insert("node_id".into(), f.get("id").cloned().unwrap_or(Value::Null));
                m.insert("path".into(), path_v.clone());
                rows.push(Value::Object(m));
            }
        }
    }

    let options = json!({ "capture": input.get("capture").cloned().unwrap_or(Value::Null) });
    Ok(writeEntryPoints(db, &origin, &json!(rows), &options))
}

fn copy_key(dst: &mut Map<String, Value>, src: &Value, key: &str) {
    if let Some(v) = src.as_object().and_then(|o| o.get(key)) { dst.insert(key.to_string(), v.clone()); }
}
// `{ kind: r.kind, subtype: r.subtype, label: r.label, path: r.path, http_method: r.http_method
// ?? null, line: r.line }` — direct property reads omit an absent key (gotcha #3); http_method
// alone gets the `??` default. Field order matches the JS literal (stored verbatim as a TEXT
// column, so insertion order is part of `db_after`).
fn snapshot_entry(r: &Value) -> Value {
    let mut m = Map::new();
    copy_key(&mut m, r, "kind");
    copy_key(&mut m, r, "subtype");
    copy_key(&mut m, r, "label");
    copy_key(&mut m, r, "path");
    m.insert("http_method".into(), nz(r.get("http_method")).cloned().unwrap_or(Value::Null));
    copy_key(&mut m, r, "line");
    Value::Object(m)
}

/// `export function writeEntryPoints(db, origin, rows, { capture = false, writeRows = true } =
/// {})` — the WRITE half: replace `code_entry_points` for `origin` (unless `writeRows` is false),
/// aggregate `counts`/`by_path`, and optionally capture a point-in-time snapshot with a diff vs
/// the prior one.
pub fn writeEntryPoints(db: &Connection, origin: &Value, rows: &Value, options: &Value) -> Value {
    let opts = options.as_object().cloned().unwrap_or_default();
    let capture = truthy(opts.get("capture"));
    let write_rows = match opts.get("writeRows") { Some(v) => truthy(Some(v)), None => true };
    let rows_arr: Vec<Value> = rows.as_array().cloned().unwrap_or_default();

    if write_rows {
        let _ = db.execute_batch("SAVEPOINT ep_batch");
        let result: Result<(), String> = (|| {
            crate::db_json::run_write(db, "DELETE FROM code_entry_points WHERE origin = ?", &[origin.clone()])?;
            for r in &rows_arr {
                crate::db_json::run_write(
                    db,
                    "INSERT INTO code_entry_points (origin, kind, subtype, node_id, path, label, http_method, detail, confidence, line, framework) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    &[
                        origin.clone(),
                        r.get("kind").cloned().unwrap_or(Value::Null),
                        r.get("subtype").cloned().unwrap_or(Value::Null),
                        r.get("node_id").cloned().unwrap_or(Value::Null),
                        r.get("path").cloned().unwrap_or(Value::Null),
                        r.get("label").cloned().unwrap_or(Value::Null),
                        nz(r.get("http_method")).cloned().unwrap_or(Value::Null),
                        nz(r.get("detail")).cloned().unwrap_or(Value::Null),
                        r.get("confidence").cloned().unwrap_or(Value::Null),
                        nz(r.get("line")).cloned().unwrap_or(Value::Null),
                        nz(r.get("framework")).cloned().unwrap_or(Value::Null),
                    ],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => { let _ = db.execute_batch("RELEASE ep_batch"); }
            Err(_) => { let _ = db.execute_batch("ROLLBACK TO ep_batch"); let _ = db.execute_batch("RELEASE ep_batch"); }
        }
    }

    // `const counts = { route: 0, ui: 0, data: 0, outbound: 0, config: 0 };` — fixed key order,
    // then `counts[r.kind] = (counts[r.kind] ?? 0) + 1` for every row (a novel kind would append).
    let mut counts = Map::new();
    for k in ["route", "ui", "data", "outbound", "config"] { counts.insert(k.to_string(), json!(0i64)); }
    let mut by_path: Map<String, Value> = Map::new();
    for r in &rows_arr {
        let kind_key = tmpl(r.get("kind")); // JS bracket-property coercion, not `?? ''`
        let cur = counts.get(&kind_key).and_then(Value::as_i64).unwrap_or(0);
        counts.insert(kind_key, json!(cur + 1));

        let path_key = tmpl(r.get("path"));
        let entry = json!({
            "kind": r.get("kind").cloned().unwrap_or(Value::Null),
            "subtype": r.get("subtype").cloned().unwrap_or(Value::Null),
            "label": r.get("label").cloned().unwrap_or(Value::Null),
            "http_method": nz(r.get("http_method")).cloned().unwrap_or(Value::Null),
            "line": r.get("line").cloned().unwrap_or(Value::Null),
        });
        by_path.entry(path_key).or_insert_with(|| json!([])).as_array_mut().unwrap().push(entry);
    }

    let mut snapshot = Value::Null;
    if capture {
        let key = |e: &Value| format!("{}|{}|{}", tmpl(e.get("kind")), tmpl(e.get("label")), tmpl(e.get("path")));
        // `[...prior]` iterates a JS Set in insertion order — Rust's HashSet has none, so track
        // first-seen order in a parallel Vec (a repeat key is a no-op, same as `Set.add`).
        let mut prior_order: Vec<String> = Vec::new();
        let mut prior_set: HashSet<String> = HashSet::new();
        if let Ok(snap_rows) = crate::db_json::query_rows(db, "SELECT entry_points_json FROM code_entry_point_snapshots WHERE origin = ? ORDER BY id DESC LIMIT 1", &[origin.clone()]) {
            if let Some(row0) = snap_rows.as_array().and_then(|a| a.first()) {
                let txt = row0.get("entry_points_json").and_then(Value::as_str).unwrap_or("[]");
                if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(txt) {
                    for e in &arr {
                        let k = key(e);
                        if prior_set.insert(k.clone()) { prior_order.push(k); }
                    }
                }
            }
        }
        let cur: HashSet<String> = rows_arr.iter().map(|r| key(r)).collect();

        let added: Vec<Value> = rows_arr.iter().filter(|r| !prior_set.contains(&key(r))).map(|r| {
            let mut m = Map::new();
            copy_key(&mut m, r, "kind");
            copy_key(&mut m, r, "label");
            copy_key(&mut m, r, "path");
            Value::Object(m)
        }).collect();
        let removed: Vec<Value> = prior_order.iter().filter(|k| !cur.contains(*k)).map(|k| {
            let parts: Vec<&str> = k.split('|').collect();
            let kind = parts.first().copied().unwrap_or("");
            let label = parts.get(1).copied().unwrap_or("");
            let path = if parts.len() > 2 { parts[2..].join("|") } else { String::new() };
            json!({ "kind": kind, "label": label, "path": path })
        }).collect();

        let ep_json_arr: Vec<Value> = rows_arr.iter().map(snapshot_entry).collect();
        let ep_json = serde_json::to_string(&Value::Array(ep_json_arr)).unwrap_or_else(|_| "[]".into());
        let mut counts_full = counts.clone();
        counts_full.insert("total".into(), json!(rows_arr.len() as i64));
        let counts_json = serde_json::to_string(&Value::Object(counts_full.clone())).unwrap_or_else(|_| "{}".into());
        let diff_json = serde_json::to_string(&json!({ "added": added, "removed": removed })).unwrap_or_else(|_| "{}".into());

        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO code_entry_point_snapshots (origin, counts_json, entry_points_json, diff_json) VALUES (?, ?, ?, ?)",
            &[origin.clone(), json!(counts_json), json!(ep_json), json!(diff_json)],
        );
        snapshot = json!({ "counts": Value::Object(counts_full), "added": added.len() as i64, "removed": removed.len() as i64 });
    }

    json!({ "origin": origin.clone(), "total": rows_arr.len() as i64, "counts": Value::Object(counts), "by_path": Value::Object(by_path), "snapshot": snapshot })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_routes_data_ui_together() {
        let src = "\n@Controller('api/v1')\nexport class FooController {\n  @Get('cards/:id') getCard() {}\n}\nconst a = db.prepare('SELECT id FROM cards WHERE slug = ?').get(slug);\nfunction Form() { return <form onSubmit={submit}><input name=\"q\" /></form>; }\n";
        let eps = detectEntryPoints(&json!(src));
        let arr = eps.as_array().unwrap();
        assert!(arr.iter().any(|e| e["kind"] == json!("route") && e["http_method"] == json!("GET") && e["label"] == json!("/api/v1/cards/:id")));
        assert!(arr.iter().any(|e| e["kind"] == json!("data") && e["subtype"] == json!("db-read") && e["label"] == json!("SELECT cards")));
        assert!(arr.iter().any(|e| e["kind"] == json!("ui") && e["subtype"] == json!("form-submit")));
    }

    #[test]
    fn detect_outbound_and_config_dedup() {
        let src = "const r = fetch('https://api.stripe.com/v1/charges'); const key = process.env.STRIPE_KEY; const again = process.env.STRIPE_KEY;";
        let eps = detectEntryPoints(&json!(src));
        let arr = eps.as_array().unwrap();
        assert!(arr.iter().any(|e| e["kind"] == json!("outbound") && e["label"] == json!("api.stripe.com")));
        assert_eq!(arr.iter().filter(|e| e["kind"] == json!("config") && e["label"] == json!("STRIPE_KEY")).count(), 1);
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE code_nodes (id TEXT, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL); \
             CREATE TABLE code_entry_points (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, kind TEXT NOT NULL, \
             subtype TEXT NOT NULL, node_id TEXT, path TEXT NOT NULL, label TEXT NOT NULL, http_method TEXT, detail TEXT, \
             confidence REAL NOT NULL DEFAULT 0.7, line INTEGER, framework TEXT, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE code_entry_point_snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, \
             captured_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', counts_json TEXT NOT NULL DEFAULT '{}', \
             entry_points_json TEXT NOT NULL DEFAULT '[]', diff_json TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn atlas_entry_points_scans_real_files_and_writes() {
        let c = db();
        const ROOT: &str = concat!(env!("ANTILOKI_FIXTURES"), "/atlas-entry-points-basic");
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('w', ?1, 1)", rusqlite::params![ROOT]).unwrap();
        for (id, path) in [
            ("n1", "src/api/cards.controller.ts"),
            ("n2", "src/db/cards.js"),
            ("n3", "src/components/Form.jsx"),
            ("n4", "src/missing.js"),
        ] {
            c.execute("INSERT INTO code_nodes (id, origin, path, type) VALUES (?1, 'atlas', ?2, 'file')", rusqlite::params![id, path]).unwrap();
        }
        let r = atlasEntryPoints(&json!({}), &c).unwrap();
        assert_eq!(r["origin"], json!("atlas"));
        assert!(r["total"].as_i64().unwrap() > 0);
        assert!(r["counts"]["route"].as_i64().unwrap() >= 1);
        let stored = crate::db_json::query_rows(&c, "SELECT count(*) n FROM code_entry_points WHERE origin='atlas'", &[]).unwrap();
        assert_eq!(stored.as_array().unwrap()[0]["n"], r["total"]);
    }

    #[test]
    fn write_entry_points_replaces_by_origin_and_counts() {
        let c = db();
        let rows = json!([
            { "kind": "route", "subtype": "http-route", "framework": "express", "http_method": "GET", "label": "/health", "confidence": 0.82, "line": 1, "node_id": "n1", "path": "a.js" },
            { "kind": "ui", "subtype": "click", "framework": "react", "label": "onClick", "confidence": 0.62, "line": 2, "node_id": "n1", "path": "a.js" },
        ]);
        let r = writeEntryPoints(&c, &json!("atlas"), &rows, &json!({}));
        assert_eq!(r["total"], json!(2));
        assert_eq!(r["counts"]["route"], json!(1));
        assert_eq!(r["counts"]["ui"], json!(1));
        assert_eq!(r["by_path"]["a.js"].as_array().unwrap().len(), 2);
        assert_eq!(r["snapshot"], Value::Null);

        let count = crate::db_json::query_rows(&c, "SELECT count(*) n FROM code_entry_points WHERE origin='atlas'", &[]).unwrap();
        assert_eq!(count.as_array().unwrap()[0]["n"], json!(2));
    }

    #[test]
    fn write_entry_points_captures_a_first_snapshot_with_everything_added() {
        let c = db();
        let rows = json!([{ "kind": "route", "subtype": "http-route", "framework": "express", "http_method": "GET", "label": "/health", "confidence": 0.82, "line": 1, "node_id": "n1", "path": "a.js" }]);
        let r = writeEntryPoints(&c, &json!("atlas"), &rows, &json!({ "capture": true }));
        assert_eq!(r["snapshot"]["added"], json!(1));
        assert_eq!(r["snapshot"]["removed"], json!(0));
        let snap = crate::db_json::query_rows(&c, "SELECT count(*) n FROM code_entry_point_snapshots WHERE origin='atlas'", &[]).unwrap();
        assert_eq!(snap.as_array().unwrap()[0]["n"], json!(1));
    }
}
