//! constitution-agent-run.rs — twin of skills/constitution/constitution-agent-run.mjs. NAMES ARE
//! CONTRACT: `constitutionAgentRun` (default export) · `extractJson` (the JS file's only other
//! `export`, the salvage-parser for a possibly-truncated/double-wrapped model reply).
//!
//! BESPOKE-NATIVE (lane A — calls `aiCall`). Reuses already-converted twins verbatim:
//! `crate::constitution__helpers_schema::ensureConstitutionSchema`,
//! `crate::engine_workspace_root::getWorkspaceRoot`,
//! `crate::constitution_constitution_agents::constitutionAgents`,
//! `crate::constitution_constitution_file::constitutionFile`,
//! `crate::constitution_constitution_neighborhood::constitutionNeighborhood`,
//! `crate::notebooks_notebook_corpus::gather`,
//! `crate::ai_prompt_find::promptFind` / `crate::ai_prompt_add::promptAdd`,
//! `crate::ai_ai_call::aiCall`.
//!
//! TWO independent PARITY-DEFERRED boundaries, both non-fatal in the JS itself (each is already
//! wrapped in its own try/catch there), ported the same way here:
//!   1. `constitutionResearch` (skills/constitution/constitution-research.mjs) has NO Rust twin —
//!      it is itself a real-web-fetch skill (out of scope for this file to port). Modeled as a
//!      local stub that always returns Err; the caller folds that into the SAME
//!      `researchText = '(web research failed — the study stands on the record alone)'` fallback
//!      the real JS catch produces on ANY research failure — so the deterministic middle of the
//!      function is fully reachable and testable regardless of this boundary.
//!   2. `aiCall` (crate::ai_ai_call::aiCall) is ITSELF PARITY-DEFERRED (real network AI dispatch —
//!      see that module's doc). Every deterministic gate before it — agent/lens resolution, the
//!      snapshot/file lookup, the field/baseline assembly, the citable-id set, ESTIMATE mode (no
//!      AI spent), and the retry ladder's own bookkeeping up to the first dispatch — is ported
//!      faithfully and is what this file's tests exercise, mirroring book-run.rs's precedent.
//! `runWithSession` (the swarm-marker AsyncLocalStorage wrapper) is untouched here for the same
//! reason run-session.rs's own twin left it unported: no Rust async-local-storage equivalent
//! exists in this harness, and it wraps nothing but the (deferred) aiCall dispatch anyway — so
//! omitting the wrapper changes no observable behavior of this twin.
//!
//! `extractJson`'s truncation-salvage walk and `validateReview`'s contract enforcement are BOTH
//! pure (no `db`, no network) and ported byte-for-byte — these are the two densest, most
//! fixture-worthy units in the file and get full native test coverage below.
// QUALITY: mixed unit — 2 pure, richly-tested helpers (extractJson, validateReview) + 1
// bespoke/deferred orchestration default export (network dispatch via aiCall, no fixture/frontier).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// ── standard helper functions (playbook) ────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

const LEVELS: [&str; 5] = ["observation", "finding", "insight", "discovery", "revelation"];
const CARD_TYPES: [&str; 6] = ["discovery", "root-cause", "tradeoff", "repair", "risk", "question"];
const SEVS: [&str; 3] = ["info", "warn", "error"];
const WIDGETS: [&str; 8] = ["graph-focus", "before-after", "repair-order", "root-cause-tree", "timeline", "flow", "balance", "layers"];
const IMPACTS: [&str; 3] = ["small", "medium", "large"];
const EFFORTS: [&str; 5] = ["xs", "s", "m", "l", "xl"];

// `const pick = (v, list, fb) => (list.includes(v) ? v : fb);` — string-domain version (every
// call site in this file picks among string enums).
fn pick(v: Option<&str>, list: &[&str], fb: &str) -> String {
    match v { Some(s) if list.contains(&s) => s.to_string(), _ => fb.to_string() }
}
// `const clip = (v, n) => (v == null ? null : String(v).slice(0, n));`
fn clip(v: Option<&Value>, n: usize) -> Option<String> {
    match v { None | Some(Value::Null) => None, Some(x) => Some(js_string(x).chars().take(n).collect()) }
}

fn wrapper_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"^\s*\{\s*"review_json"\s*:\s*"?"#).unwrap())
}

/// `export function extractJson(text, depth2 = 0)` — pure salvage parser: strips a
/// `{"review_json": "…"}` double-wrap (recursing at most twice), then tries (a) largest
/// `{…}` substring by first/last brace, (b) a balanced-from-start scan, (c) a truncation salvage
/// that retreats to the last cleanly-closed value, drops a dangling key, and closes every open
/// brace/bracket. Nothing invented — only closed or dropped.
pub fn extractJson(text: &Value, depth2: u32) -> Option<Value> {
    let s = str_qq(Some(text));
    if depth2 < 2 {
        if let Some(m) = wrapper_re().find(&s) {
            let inner_raw = &s[m.end()..];
            // `.replace(/"?\s*\}\s*$/, '')` — strip a trailing (optional-quote) `}` with leading ws.
            let end_re = {
                static R: OnceLock<Regex> = OnceLock::new();
                R.get_or_init(|| Regex::new(r#""?\s*\}\s*$"#).unwrap())
            };
            let stripped = end_re.replace(inner_raw, "").to_string();
            let unescaped = stripped.replace("\\\"", "\"").replace("\\n", "\n");
            if let Some(r) = extractJson(&json!(unescaped), depth2 + 1) {
                return Some(r);
            }
        }
    }
    let bytes: Vec<char> = s.chars().collect();
    let a = match bytes.iter().position(|&c| c == '{') { Some(i) => i, None => return None };
    let b = bytes.iter().rposition(|&c| c == '}');
    if let Some(b) = b {
        if b > a {
            let slice: String = bytes[a..=b].iter().collect();
            if let Ok(v) = serde_json::from_str::<Value>(&slice) { return Some(v); }
        }
    }
    // balanced-prefix: the largest closed object from the start
    let mut depth = 0i64;
    let mut in_str = false;
    let mut esc = false;
    let mut end: Option<usize> = None;
    for i in a..bytes.len() {
        let ch = bytes[i];
        if in_str {
            if esc { esc = false; } else if ch == '\\' { esc = true; } else if ch == '"' { in_str = false; }
            continue;
        }
        if ch == '"' { in_str = true; }
        else if ch == '{' { depth += 1; }
        else if ch == '}' { depth -= 1; if depth == 0 { end = Some(i); break; } }
    }
    if let Some(end) = end {
        if end > a {
            let slice: String = bytes[a..=end].iter().collect();
            if let Ok(v) = serde_json::from_str::<Value>(&slice) { return Some(v); }
        }
    }
    // TRUNCATION salvage
    let try_close = |idx: usize| -> Option<Value> {
        if idx <= a { return None; }
        let mut st: Vec<char> = Vec::new();
        let mut ins = false;
        let mut es = false;
        for i in a..=idx {
            let ch = bytes[i];
            if ins {
                if es { es = false; } else if ch == '\\' { es = true; } else if ch == '"' { ins = false; }
                continue;
            }
            if ch == '"' { ins = true; }
            else if ch == '{' || ch == '[' { st.push(ch); }
            else if ch == '}' || ch == ']' { st.pop(); }
        }
        if ins || st.is_empty() { return None; }
        let mut head: String = bytes[a..=idx].iter().collect();
        let dangling_key_re = {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new(r#",?\s*"(?:[^"\\]|\\.)*"\s*:?\s*$"#).unwrap())
        };
        head = dangling_key_re.replace(&head, "").to_string();
        let trailing_comma_re = {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new(r#",\s*$"#).unwrap())
        };
        head = trailing_comma_re.replace(&head, "").to_string();
        let closers: String = st.iter().rev().map(|&c| if c == '{' { '}' } else { ']' }).collect();
        serde_json::from_str::<Value>(&format!("{head}{closers}")).ok()
    };
    // find the last position that closes a value cleanly
    in_str = false;
    esc = false;
    let mut last_good: i64 = -1;
    for i in a..bytes.len() {
        let ch = bytes[i];
        if in_str {
            if esc { esc = false; } else if ch == '\\' { esc = true; } else if ch == '"' { in_str = false; last_good = i as i64; }
            continue;
        }
        if ch == '"' { in_str = true; }
        else if ch == '{' || ch == '[' { /* opener — not a cut point */ }
        else if ch == '}' || ch == ']' { last_good = i as i64; }
        else if ch.is_ascii_digit() || "truefalsn.".contains(ch) { last_good = i as i64; }
    }
    let mut idx = last_good;
    for _ in 0..5 {
        if idx <= a as i64 { break; }
        if let Some(r) = try_close(idx as usize) { return Some(r); }
        // `s.lastIndexOf(',', idx - 1)` — search char-by-char since we're in a char Vec.
        let mut found = -1i64;
        let mut j = idx - 1;
        while j >= a as i64 {
            if bytes[j as usize] == ',' { found = j; break; }
            j -= 1;
        }
        idx = found;
    }
    None
}

/// THE VALIDATOR — deterministic enforcement of the contract. Returns `(review_or_null,
/// validation)`. Citations checked against `citable`; unknown widgets become plain cards; a
/// theory nobody cited demotes to an unsupported hypothesis.
fn validate_review(body: Option<&Value>, citable: &HashSet<String>) -> (Value, Value) {
    let empty_validation = |parsed: bool| json!({ "parsed": parsed, "uncited_dropped": 0, "widgets_converted": 0 });
    let body = match body {
        Some(b) if b.is_object() => b,
        _ => return (Value::Null, empty_validation(false)),
    };
    if body.get("summary").is_none() && body.get("theory").is_none() {
        return (Value::Null, empty_validation(false));
    }
    let mut uncited = 0i64;
    let mut converted = 0i64;

    let evidence: Vec<Value> = body.get("evidence").and_then(Value::as_array).cloned().unwrap_or_default()
        .into_iter().take(8)
        .filter_map(|e| {
            let id = js_string(e.get("id").unwrap_or(&Value::Null));
            let ok = e.is_object() && citable.contains(&id);
            if !ok { uncited += 1; return None; }
            let kind = if Regex::new(r"^https?:").unwrap().is_match(&id) { "web" }
                else if id.contains('/') { "file" } else { "law" };
            let kind_pick = pick(e.get("kind").and_then(Value::as_str), &["law", "axis", "file", "web"], kind);
            Some(json!({ "kind": kind_pick, "id": id }))
        }).collect();

    let mut cards: Vec<Value> = body.get("cards").and_then(Value::as_array).cloned().unwrap_or_default()
        .into_iter().take(4)
        .map(|c| {
            json!({
                "type": pick(c.get("type").and_then(Value::as_str), &CARD_TYPES, "discovery"),
                "severity": pick(c.get("severity").and_then(Value::as_str), &SEVS, "info"),
                "title": clip(c.get("title"), 120).unwrap_or_else(|| "(untitled)".to_string()),
                "body": clip(c.get("body"), 500).unwrap_or_default(),
                "confidence": c.get("confidence").and_then(Value::as_f64).map(|f| json!(f.max(0.0).min(1.0))).unwrap_or(Value::Null),
            })
        }).collect();

    let mut widgets: Vec<Value> = Vec::new();
    for w in body.get("widgets").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().take(2) {
        let wt = w.get("type").and_then(Value::as_str);
        if let Some(wt) = wt.filter(|t| WIDGETS.contains(t)) {
            widgets.push(json!({
                "type": wt,
                "title": clip(w.get("title"), 120).unwrap_or_else(|| wt.to_string()),
                "payload": w.get("payload").cloned().unwrap_or(json!({})),
            }));
        } else {
            converted += 1;
            let wtype_clip = clip(w.get("type"), 40).unwrap_or_default();
            cards.push(json!({
                "type": "question",
                "severity": "info",
                "title": clip(w.get("title"), 120).unwrap_or_else(|| "(a proposed visual)".to_string()),
                "body": format!("proposed an unsupported widget type: {wtype_clip}"),
                "confidence": Value::Null,
            }));
        }
    }

    let t = body.get("theory").filter(|v| v.is_object());
    let unsupported = evidence.is_empty();
    let theory = t.map(|t| json!({
        "name": clip(t.get("name"), 120).unwrap_or_else(|| "(unnamed theory)".to_string()),
        "explanation": clip(t.get("explanation"), 700).unwrap_or_default(),
        "root_cause": clip(t.get("root_cause"), 300),
        "engineering_story": clip(t.get("engineering_story"), 500),
        "if_nothing_changes": clip(t.get("if_nothing_changes"), 300),
        "confidence": t.get("confidence").and_then(Value::as_f64).map(|f| json!(f.max(0.0).min(1.0))).unwrap_or(Value::Null),
        "unsupported": unsupported,
    }));

    let s = body.get("summary").filter(|v| v.is_object()).cloned().unwrap_or(json!({}));
    let theory_name = theory.as_ref().and_then(|t| t.get("name")).cloned();
    let summary = json!({
        "level": if unsupported && theory.is_some() { "observation".to_string() } else { pick(s.get("level").and_then(Value::as_str), &LEVELS, "finding") },
        "title": clip(s.get("title"), 120).or_else(|| theory_name.as_ref().map(|v| js_string(v))).unwrap_or_else(|| "(untitled)".to_string()),
        "headline": clip(s.get("headline"), 200).unwrap_or_default(),
    });

    let actions: Vec<Value> = body.get("actions").and_then(Value::as_array).cloned().unwrap_or_default()
        .into_iter().take(3)
        .map(|a| json!({
            "title": clip(a.get("title"), 120).unwrap_or_else(|| "(untitled)".to_string()),
            "strategy": clip(a.get("strategy"), 300).unwrap_or_default(),
            "impact": pick(a.get("impact").and_then(Value::as_str), &IMPACTS, "small"),
            "effort": pick(a.get("effort").and_then(Value::as_str), &EFFORTS, "m"),
        })).collect();

    let lesson = body.get("lesson").filter(|l| l.is_object() && (truthy(l.get("concept")) || truthy(l.get("sample"))))
        .map(|l| json!({
            "concept": clip(l.get("concept"), 140),
            "sample": clip(Some(&json!(js_string(l.get("sample").unwrap_or(&Value::Null)))), 700),
            "caption": clip(l.get("caption"), 220),
        })).unwrap_or(Value::Null);

    let review = json!({
        "summary": summary, "theory": theory, "cards": cards, "evidence": evidence,
        "widgets": widgets, "actions": actions,
        "expected_pushback": clip(body.get("expected_pushback"), 200),
        "lesson": lesson,
    });
    (review, json!({ "parsed": true, "uncited_dropped": uncited, "widgets_converted": converted }))
}

// Node `path.resolve(input)` (single positional arg): join against cwd then lexically normalize
// `.`/`..` — mirrors constitution-neighborhood.rs's private `node_resolve` (no symlink
// resolution, matching Node's own purely-lexical behavior). Duplicated locally since the sibling
// twin's copy is private (`fn`, not `pub fn`).
fn node_resolve(input: &str) -> String {
    let base = if Path::new(input).is_absolute() { PathBuf::new() } else { std::env::current_dir().unwrap_or_default() };
    let combined = base.join(input);
    let mut out = PathBuf::new();
    for comp in combined.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => { out.pop(); }
            c => out.push(c.as_os_str()),
        }
    }
    out.to_string_lossy().replace('\\', "/")
}
fn node_join(root: &str, rel: &str) -> String {
    let mut p = PathBuf::from(root);
    p.push(rel);
    p.to_string_lossy().replace('\\', "/")
}

// PARITY-DEFERRED: constitution-research.mjs (real web fetch/scrape) has no Rust twin — see
// module doc. Always Err; every call site folds this into the SAME fallback string the JS
// try/catch produces on any research failure, so it is non-fatal to the deterministic run.
fn constitution_research_stub(_queries: &Value, _urls: &Value, _db: &Connection) -> Result<Value, String> {
    Err("constitution-agent-run twin: constitution-research has no Rust twin yet (real web fetch/scrape) — PARITY-DEFERRED".into())
}

pub fn constitutionAgentRun(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let agent_id = str_qq(input.get("agent_id")).trim().to_string();
    if agent_id.is_empty() { return Err("constitution-agent-run: agent_id is required".into()); }
    let path = str_qq(input.get("path")).replace('\\', "/").trim().to_string();
    if path.is_empty() { return Err("constitution-agent-run: path is required".into()); }
    let root = if truthy(input.get("root")) { node_resolve(&str_qq(input.get("root"))) } else { js_string(&crate::engine_workspace_root::getWorkspaceRoot(db)) };

    // 'custom' = the inline ad-hoc specialist: the operator's instruction IS the lens
    let lens: Value;
    if agent_id == "custom" {
        let instruction = str_qq(input.get("custom_instruction")).trim().to_string();
        if instruction.is_empty() { return Err("constitution-agent-run: custom_instruction (for agent_id \"custom\") is required".into()); }
        let title: String = if truthy(input.get("custom_title")) { js_string(input.get("custom_title").unwrap()) } else { "Custom Reviewer".to_string() };
        let title: String = title.chars().take(60).collect();
        let description: String = instruction.chars().take(600).collect();
        lens = json!({ "id": "custom", "title": title, "category": "custom", "description": description, "laws": [], "axes": [], "ready": true, "readiness": "ready" });
    } else {
        let reg = crate::constitution_constitution_agents::constitutionAgents(&json!({}), db)?;
        let found = reg.get("agents").and_then(Value::as_array).and_then(|a| a.iter().find(|x| x.get("id").and_then(Value::as_str) == Some(agent_id.as_str())).cloned());
        let found = match found { Some(f) => f, None => return Ok(json!({ "ok": false, "error": format!("no such specialist in this workspace's registry: {agent_id}") })) };
        if found.get("ready") != Some(&Value::Bool(true)) {
            let readiness = js_string(found.get("readiness").unwrap_or(&Value::Null));
            let title = js_string(found.get("title").unwrap_or(&Value::Null));
            return Ok(json!({ "ok": false, "error": format!("{title} is not ready — {readiness}") }));
        }
        lens = found;
    }

    let mut fc = crate::constitution_constitution_file::constitutionFile(&json!({ "path": path.clone() }), db)?;
    // A FILE OUTSIDE THE LAST SNAPSHOT (2026-09-19): the specialist still reads it — the measured record
    // is just empty (no health, no axes, no recorded violations) and the verdict says so. Refusing made
    // every sub-agent brief fail on repos where the snapshot judged a subset (1,200 of 4,579 files).
    // A path that is not a file on disk is still refused.
    if fc.get("found") != Some(&Value::Bool(true)) {
        let root = crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default().to_string();
        let on_disk = std::path::Path::new(&root).join(&path);
        if input.get("estimate") != Some(&Value::Bool(true)) && !on_disk.is_file() {
            return Ok(json!({ "ok": false, "error": format!("{path} is outside the latest snapshot and not a file in the workspace") }));
        }
        fc = json!({ "path": path.clone(), "found": false, "unjudged": true, "health": Value::Null, "score": Value::Null, "axes": {}, "violations": [], "is_folder": false });
    }
    let lens_laws: Vec<String> = lens.get("laws").and_then(Value::as_array).cloned().unwrap_or_default().iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let lens_law_set: HashSet<String> = lens_laws.iter().cloned().collect();
    let all_viols: Vec<Value> = fc.get("violations").and_then(Value::as_array).cloned().unwrap_or_default();
    let viols: Vec<Value> = all_viols.iter()
        .filter(|v| lens_law_set.is_empty() || lens_law_set.contains(v.get("law_key").and_then(Value::as_str).unwrap_or("")))
        .take(12).cloned().collect();
    let viols_text = if viols.is_empty() {
        "(none recorded for this lens — interpret the axes and the source)".to_string()
    } else {
        viols.iter().map(|v| {
            let key = js_string(v.get("law_key").unwrap_or(&Value::Null));
            let sev = js_string(v.get("severity").unwrap_or(&Value::Null));
            let line = v.get("line").filter(|x| !x.is_null()).map(|l| format!(" · L{}", js_string(l))).unwrap_or_default();
            let ev = v.get("evidence").map(|e| js_string(e)).unwrap_or_default();
            format!("{key} · {sev}{line} · «{ev}»")
        }).collect::<Vec<_>>().join("\n")
    };
    let laws_text = {
        let ks: Vec<String> = lens_laws.iter().take(20).cloned().collect();
        if ks.is_empty() { "(a measured lens — no laws, interpret the axes)".to_string() }
        else {
            ks.iter().map(|k| {
                let v = all_viols.iter().find(|x| x.get("law_key").and_then(Value::as_str) == Some(k.as_str()));
                let rule = v.and_then(|v| v.get("rule_md")).filter(|x| !x.is_null())
                    .or_else(|| v.and_then(|v| v.get("title")))
                    .map(|x| js_string(x)).unwrap_or_default();
                format!("{k} · {rule}").trim().to_string()
            }).collect::<Vec<_>>().join("\n")
        }
    };
    let axes_text = {
        let axes_arr: Vec<String> = lens.get("axes").and_then(Value::as_array).cloned().unwrap_or_default().iter().filter_map(|v| v.as_str().map(String::from)).collect();
        axes_arr.iter().map(|ax| {
            match fc.get("axes").and_then(|a| a.get(ax)) {
                Some(a) if !a.is_null() => format!("{ax}={}", js_string(a.get("score").unwrap_or(&Value::Null))),
                _ => format!("{ax}=?"),
            }
        }).collect::<Vec<_>>().join(" · ")
    };

    let is_folder = fc.get("is_folder") == Some(&Value::Bool(true));
    let mut field_text = "(no field — the case stands alone in the snapshot)".to_string();
    let mut neighbor_paths: HashSet<String> = HashSet::new();
    if !is_folder {
        if let Ok(nb) = crate::constitution_constitution_neighborhood::constitutionNeighborhood(&json!({ "path": path.clone(), "depth": 1, "max_per_ring": 6 }), db) {
            if nb.get("ok") != Some(&Value::Bool(false)) {
                if let Some(nodes) = nb.get("nodes").and_then(Value::as_array) {
                    let dir_label = |d: &str| -> String { match d { "up" => "imports the case (upstream)".to_string(), "down" => "the case leans on (downstream)".to_string(), "issue" => "same law elsewhere (a codebase echo)".to_string(), other => other.to_string() } };
                    let mut lines: Vec<String> = Vec::new();
                    let edges: Vec<Value> = nb.get("edges").and_then(Value::as_array).cloned().unwrap_or_default();
                    for n in nodes.iter().filter(|x| x.get("dir").and_then(Value::as_str) != Some("center")).take(12) {
                        let npath = js_string(n.get("path").unwrap_or(&Value::Null));
                        neighbor_paths.insert(npath.clone());
                        let dir = n.get("dir").and_then(Value::as_str).unwrap_or("");
                        let e = edges.iter().find(|x| {
                            (x.get("from").and_then(Value::as_str) == Some(npath.as_str()) && x.get("to").and_then(Value::as_str) == Some(path.as_str()))
                                || (x.get("from").and_then(Value::as_str) == Some(path.as_str()) && x.get("to").and_then(Value::as_str) == Some(npath.as_str()))
                        });
                        let via_s = e.and_then(|e| {
                            if let Some(via) = e.get("via").and_then(Value::as_array).filter(|a| !a.is_empty()) {
                                let items: Vec<String> = via.iter().take(4).map(js_string).collect();
                                Some(format!(" · carries: {}", items.join(", ")))
                            } else if e.get("kind").and_then(Value::as_str) == Some("co_change") {
                                Some(format!(" · co-changes ×{}", js_string(e.get("n").unwrap_or(&Value::Null))))
                            } else { None }
                        }).unwrap_or_default();
                        let health = n.get("health").filter(|x| !x.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
                        let worst = n.get("worst_law").filter(|x| !x.is_null()).map(|x| format!(" · worst § {}", js_string(x))).unwrap_or_default();
                        lines.push(format!("{npath} — {} · health {health}{worst}{via_s}", dir_label(dir)));
                    }
                    if let Some(origin) = nb.get("origin_info").filter(|x| !x.is_null()) {
                        let file = origin.get("file").filter(|x| !x.is_null()).map(js_string).unwrap_or_else(|| path.clone());
                        let reason = origin.get("reason").filter(|x| !x.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
                        let commit = origin.get("first_commit").filter(|x| !x.is_null()).map(|c| format!(" · first commit {}", js_string(c).chars().take(9).collect::<String>())).unwrap_or_default();
                        lines.push(format!("origin traces to: {file} · reason {reason}{commit}"));
                    }
                    if !lines.is_empty() { field_text = lines.join("\n"); }
                }
            }
        }
    }

    // the POPULATION baseline
    let mut baseline_text = "(no baseline — no other recorded holders of these laws)".to_string();
    if let Ok(snap_rows) = crate::db_json::query_rows(db, "SELECT id FROM judgments.review_snapshots WHERE status='done' ORDER BY id DESC LIMIT 1", &[]) {
        if let Some(snap_row) = snap_rows.as_array().and_then(|a| a.first()) {
            let snap_id = snap_row.get("id").cloned().unwrap_or(Value::Null);
            let mut case_laws: Vec<String> = Vec::new();
            for v in &all_viols {
                let k = js_string(v.get("law_key").unwrap_or(&Value::Null));
                if !case_laws.contains(&k) { case_laws.push(k); }
            }
            case_laws.truncate(8);
            if !case_laws.is_empty() {
                let mut lines: Vec<String> = Vec::new();
                for k in &case_laws {
                    let n_in_case = all_viols.iter().filter(|v| v.get("law_key").and_then(Value::as_str) == Some(k.as_str())).count();
                    let r = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n, COUNT(DISTINCT path) AS files FROM judgments.review_violations WHERE snapshot_id = ? AND law_key = ?", &[snap_id.clone(), json!(k)]).ok()
                        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));
                    let n = r.as_ref().and_then(|r| r.get("n")).cloned().unwrap_or(json!(0));
                    let files = r.as_ref().and_then(|r| r.get("files")).cloned().unwrap_or(json!(0));
                    lines.push(format!("§ {k}: {n_in_case} in the case · {} across {} file(s) in the region", js_string(&n), js_string(&files)));
                }
                baseline_text = lines.join("\n");
            }
        }
    }

    // the CITABLE universe
    let mut citable: HashSet<String> = HashSet::new();
    for k in &lens_laws { citable.insert(k.clone()); }
    for v in &all_viols { citable.insert(js_string(v.get("law_key").unwrap_or(&Value::Null))); }
    if let Some(ax) = lens.get("axes").and_then(Value::as_array) { for a in ax { citable.insert(js_string(a)); } }
    if let Some(ax) = fc.get("axes").and_then(Value::as_object) { for k in ax.keys() { citable.insert(k.clone()); } }
    for p in &neighbor_paths { citable.insert(p.clone()); }

    const MAX_SOURCE: usize = 5000;
    let source = if !is_folder {
        let full_path = if Path::new(&path).is_absolute() { path.clone() } else { node_join(&root, &path) };
        std::fs::read_to_string(&full_path).map(|s| s.chars().take(MAX_SOURCE).collect::<String>()).unwrap_or_else(|_| "(unreadable)".to_string())
    } else {
        let files = fc.get("files").filter(|x| !x.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
        format!("(a folder of {files} files — interpret the aggregate verdict and violations)")
    };

    // ADDITIONAL CONTEXT
    let mut sources_text = "(no additional sources selected)".to_string();
    let source_kinds: Vec<String> = input.get("sources").and_then(Value::as_array).cloned().unwrap_or_default()
        .iter().map(js_string).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).take(12).collect();
    if !source_kinds.is_empty() {
        let gscope = json!({ "paths": if is_folder { json!([]) } else { json!([path.clone()]) } });
        let mut blocks: Vec<String> = Vec::new();
        for k in &source_kinds {
            // `pack:<slug>` = a context pack from the library (2026-09-19) — the whole pack rides, not a 1.5k slice
            let (src, cap) = match k.strip_prefix("pack:") { Some(slug) => (json!({ "kind": "context_pack", "path": slug }), 20_000usize), None => (json!({ "kind": k }), 1500usize) };
            let t = crate::notebooks_notebook_corpus::gather(db, &src, &gscope, &json!({}));
            let ts = js_string(&t);
            if !ts.trim().is_empty() { blocks.push(format!("### {k}\n{}", ts.chars().take(cap).collect::<String>())); }
        }
        let total_cap = if source_kinds.iter().any(|k| k.starts_with("pack:")) { 40_000 } else { 6000 };
        if !blocks.is_empty() { sources_text = blocks.join("\n\n").chars().take(total_cap).collect(); }
    }

    // THE RUN-ON CHOICE (operator 2026-09-19): `tier` (low|mid|high; the template's mid otherwise) and
    // `engine` (claude-code|codex|opencode; the tier's active row otherwise) — for the run and the estimate.
    let run_tier: Option<String> = input.get("tier").and_then(Value::as_str).map(|t| t.trim().to_ascii_lowercase()).filter(|t| ["low", "mid", "high"].contains(&t.as_str()));
    let run_engine: Option<String> = input.get("engine").and_then(Value::as_str).map(|e| e.trim().trim_start_matches("cli:").to_string()).filter(|e| ["claude-code", "codex", "opencode"].contains(&e.as_str()));

    // ── ESTIMATE mode: assemble the REAL prompt, count its tokens, resolve the route — no AI spent
    if input.get("estimate") == Some(&Value::Bool(true)) {
        let title = js_string(lens.get("title").unwrap_or(&Value::Null));
        let category = js_string(lens.get("category").unwrap_or(&Value::Null));
        let description = js_string(lens.get("description").unwrap_or(&Value::Null));
        let lens_line = format!("{title} ({category}) — {description}");
        let citable_join = citable.iter().cloned().collect::<Vec<_>>().join(", ");
        // the prompt's own weight — the real prose registered by ensure_prompt
        let system_len: usize = REVIEW_SYSTEM.chars().count();
        let template_len: usize = REVIEW_TEMPLATE.chars().count();
        #[allow(non_snake_case)] let SYSTEM_LEN = system_len;
        #[allow(non_snake_case)] let TEMPLATE_LEN = template_len;
        let chars = field_text.chars().count() + baseline_text.chars().count() + sources_text.chars().count()
            + SYSTEM_LEN + TEMPLATE_LEN + lens_line.chars().count() + path.chars().count()
            + laws_text.chars().count() + viols_text.chars().count() + citable_join.chars().count()
            + source.chars().count() + 80;
        let est_tokens_in = ((chars as f64) / 4.0).ceil() as i64;
        // the route the run would take: the chosen tier (default mid) and engine, or the tier's active row
        let route = route_estimate(db, run_tier.as_deref().unwrap_or("mid"), run_engine.as_deref(), est_tokens_in, 2200);
        let agent = js_string(lens.get("title").unwrap_or(&Value::Null));
        // WHAT THE TOKENS ARE (operator 2026-09-19: "it just counted the files, not all the scope"): the
        // case's source, the measured record, the attached sources, the prompt itself
        let tk = |n: usize| ((n as f64) / 4.0).ceil() as i64;
        let breakdown = json!({
            "source": tk(source.chars().count()),
            "record": tk(laws_text.chars().count() + viols_text.chars().count() + field_text.chars().count() + baseline_text.chars().count() + lens_line.chars().count() + citable_join.chars().count() + path.chars().count() + 80),
            "sources": tk(sources_text.chars().count()),
            "prompt": tk(SYSTEM_LEN + TEMPLATE_LEN),
        });
        let mut out = json!({ "ok": true, "estimate": true, "agent": agent, "path": path, "est_tokens_in": est_tokens_in, "max_output_tokens": 2200, "est_breakdown": breakdown });
        for (k, v) in route.as_object().into_iter().flatten() { out[k] = v.clone(); }
        return Ok(out);
    }

    // W1 — PRE-FLIGHT WEB RESEARCH — PARITY-DEFERRED (see module doc's stub); non-fatal.
    let mut research_text = "(web research off)".to_string();
    let mut research: Value = Value::Null;
    if input.get("research") != Some(&Value::Bool(false)) && !is_folder {
        let explicit_urls: Vec<Value> = input.get("research_urls").and_then(Value::as_array).cloned().unwrap_or_default()
            .into_iter().filter(|u| u.as_str().map_or(false, |s| s.starts_with("http://") || s.starts_with("https://"))).take(9).collect();
        let queries: Vec<Value> = if !explicit_urls.is_empty() { vec![] } else {
            let lang = if path.ends_with(".ts") || path.ends_with(".tsx") { "TypeScript" } else if path.ends_with(".py") { "Python" } else { "JavaScript" };
            let mut top_laws: Vec<String> = Vec::new();
            for v in &all_viols { let k = js_string(v.get("law_key").unwrap_or(&Value::Null)); if !top_laws.contains(&k) { top_laws.push(k); } }
            top_laws.truncate(2);
            let title_of = |k: &str| all_viols.iter().find(|v| v.get("law_key").and_then(Value::as_str) == Some(k))
                .and_then(|v| v.get("title")).filter(|t| !t.is_null()).map(js_string)
                .unwrap_or_else(|| k.replace('-', " "));
            let mut qs: Vec<Value> = top_laws.iter().map(|k| json!(format!("{} {lang} best practice", title_of(k)))).collect();
            let axes_arr: Vec<Value> = lens.get("axes").and_then(Value::as_array).cloned().unwrap_or_default();
            if !axes_arr.is_empty() && top_laws.is_empty() { qs.push(json!(format!("{} {lang} code quality", js_string(&axes_arr[0])))); }
            qs.truncate(3);
            qs
        };
        if !explicit_urls.is_empty() || !queries.is_empty() {
            match constitution_research_stub(&json!(queries), &json!(explicit_urls), db) {
                Ok(r) => {
                    let sources: Vec<Value> = r.get("sources").and_then(Value::as_array).cloned().unwrap_or_default();
                    if !sources.is_empty() {
                        research_text = sources.iter().map(|s| format!("[{}] {} — {} <{}>", js_string(s.get("domain").unwrap_or(&Value::Null)), js_string(s.get("title").unwrap_or(&Value::Null)), js_string(s.get("snippet").unwrap_or(&Value::Null)), js_string(s.get("url").unwrap_or(&Value::Null)))).collect::<Vec<_>>().join("\n");
                        for s in &sources { citable.insert(js_string(s.get("url").unwrap_or(&Value::Null))); }
                        research = r;
                    } else {
                        research_text = "(no usable web results — the study stands on the record alone)".to_string();
                    }
                }
                Err(_) => { research_text = "(web research failed — the study stands on the record alone)".to_string(); }
            }
        }
    }

    // THE SWARM MARKER — `runWithSession` is a real no-op wrapper here (PARITY-DEFERRED in
    // run-session.rs itself; see module doc): swarm_id is read but has nothing to thread into.
    let _swarm_id = input.get("swarm_id").filter(|v| !v.is_null()).map(|v| js_string(v).chars().take(80).collect::<String>());

    ensure_prompt(db)?;

    // the RETRY LADDER — every attempt is caught; a thrown aiCall is a failed attempt, never a
    // dead run. `aiCall` is PARITY-DEFERRED (always Err — see crate::ai_ai_call's module doc), so
    // this loop always exhausts all 3 attempts and `review` stays None: this mirrors the JS run's
    // real behavior whenever the network dispatch itself fails 3 times in a row.
    let lens_title = js_string(lens.get("title").unwrap_or(&Value::Null));
    let lens_category = js_string(lens.get("category").unwrap_or(&Value::Null));
    let lens_description = js_string(lens.get("description").unwrap_or(&Value::Null));
    let lens_id = js_string(lens.get("id").unwrap_or(&Value::Null));
    let health = fc.get("health").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
    let score = fc.get("score").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
    let verdict = if fc.get("unjudged") == Some(&Value::Bool(true)) { "not judged by the last X-ray snapshot — no measured health, axes or recorded violations for this file; review from the source and the laws (rebuild the X-ray to measure it)".to_string() } else { format!("health {health} · law-compliance {score} · {axes_text}") };
    let citable_join = if citable.is_empty() { "(nothing citable — an unsupported theory is acceptable here)".to_string() } else { citable.iter().cloned().collect::<Vec<_>>().join(", ") };

    // THE TRACE (operator 2026-09-19: "a log window with exactly the actions happening — agents, calls,
    // context, web scraping"): what this call assembled and did, step by step, for the console.
    let t0 = std::time::Instant::now();
    let mut trace: Vec<Value> = Vec::new();
    let step = |trace: &mut Vec<Value>, kind: &str, text: String| { trace.push(json!({ "t": t0.elapsed().as_millis() as i64, "kind": kind, "text": text })); };
    let tok = |s: &str| (s.chars().count() as f64 / 4.0).ceil() as i64;
    step(&mut trace, "context", format!("lens {lens_title} · case {path} · verdict {verdict}"));
    step(&mut trace, "context", format!("laws ≈{} tok · violations ≈{} tok · field ≈{} tok · baseline ≈{} tok · source ≈{} tok · citable {} ids", tok(&laws_text), tok(&viols_text), tok(&field_text), tok(&baseline_text), tok(&source), citable.len()));
    if !source_kinds.is_empty() { step(&mut trace, "context", format!("attached sources {} ≈{} tok", source_kinds.join(", "), tok(&sources_text))); }
    if !research.is_null() { step(&mut trace, "research", format!("web research: {} source(s)", research.get("sources").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0))); }

    let mut attempts = 0i64;
    let mut cost = 0.0f64;
    let mut ai_call_id: Value = Value::Null;
    let mut text = String::new();
    let mut review: Value = Value::Null;
    let mut validation: Value = Value::Null;
    let mut last_err: Option<String> = None;
    for att in 1..=3 {
        attempts = att;
        let mut input_kv = vec![
            json!({ "key": "lens", "value": format!("{lens_title} ({lens_category}) — {lens_description}") }),
            json!({ "key": "path", "value": path }),
            json!({ "key": "verdict", "value": verdict }),
            json!({ "key": "laws", "value": laws_text }),
            json!({ "key": "violations", "value": viols_text }),
            json!({ "key": "field", "value": field_text }),
            json!({ "key": "baseline", "value": baseline_text }),
            json!({ "key": "sources", "value": sources_text }),
            json!({ "key": "research", "value": research_text }),
            json!({ "key": "citable", "value": citable_join }),
            json!({ "key": "source", "value": source }),
        ];
        if att > 1 { input_kv.push(json!({ "key": "_retry", "value": att.to_string() })); }
        let mut call_input = json!({
            "prompt_name": "constitution-agent-review",
            "input_kv": input_kv,
            "bypass_cache": att > 1,
        });
        // THE RUN-ON CHOICE (operator 2026-09-19): the mission picks the reviewers' tier (default mid — the
        // template's) and, optionally, the coding-CLI engine (cli:<engine> pins ai-call to it).
        if let Some(t) = run_tier.as_deref() { call_input["tier"] = json!(t); }
        if let Some(e) = run_engine.as_deref() { call_input["model_id"] = json!(format!("cli:{e}")); }
        if att >= 3 { call_input["tier"] = json!("high"); }
        step(&mut trace, "call", format!("attempt {att}: ai-call constitution-agent-review · tier {}{}", call_input.get("tier").and_then(Value::as_str).unwrap_or("mid (template)"), run_engine.as_deref().map(|e| format!(" · pinned to {e}")).unwrap_or_default()));
        match crate::ai_ai_call::aiCall(&call_input, db) {
            Ok(res) => {
                cost += res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                if let Some(id) = nz(res.get("ai_call_id")) { ai_call_id = id.clone(); }
                step(&mut trace, "answer", format!("{}/{} answered in {} ms{} · ai_call #{}", js_string(res.get("provider").unwrap_or(&json!("?"))), js_string(res.get("model_id").unwrap_or(&json!("?"))), res.get("duration_ms").and_then(Value::as_f64).map(|d| d.round() as i64).unwrap_or(0), if res.get("cached") == Some(&json!(true)) { " (cache)" } else { "" }, js_string(res.get("ai_call_id").unwrap_or(&Value::Null))));
                text = res.get("output_kv").and_then(Value::as_array).and_then(|arr| arr.iter().find(|o| {
                    o.get("field_name").and_then(Value::as_str) == Some("review_json") && o.get("parent_output_id").map_or(true, |v| v.is_null())
                })).and_then(|o| o.get("value_text")).map(js_string).unwrap_or_default();
                let parsed = extractJson(&json!(text.clone()), 0);
                let (r, v) = validate_review(parsed.as_ref(), &citable);
                step(&mut trace, if r.is_null() { "reject" } else { "validate" }, if r.is_null() { format!("the reply did not parse as a review ({} chars) — retrying", text.chars().count()) } else { format!("review validated · {} uncited citation(s) dropped · {} widget(s) converted", v.get("uncited_dropped").and_then(Value::as_i64).unwrap_or(0), v.get("widgets_converted").and_then(Value::as_i64).unwrap_or(0)) });
                review = r; validation = v;
                last_err = None;
                if !review.is_null() { break; }
            }
            Err(e) => { step(&mut trace, "error", e.chars().take(220).collect()); last_err = Some(e.chars().take(220).collect()); }
        }
    }

    let dropped = if review.is_null() { 1 } else { 0 };
    let parse_excerpt: Value = if review.is_null() { json!(last_err.unwrap_or_else(|| text.chars().take(220).collect())) } else { Value::Null };
    let research_out = if research.is_null() { Value::Null } else {
        json!({ "queries": research.get("queries").cloned().unwrap_or(Value::Null), "sources": research.get("sources").cloned().unwrap_or(Value::Null) })
    };
    Ok(json!({
        "ok": true, "agent": lens_title, "agent_id": lens_id, "path": path,
        "review": review, "validation": validation, "attempts": attempts,
        "dropped": dropped, "parse_excerpt": parse_excerpt,
        "cost_usd": cost, "ai_call_id": ai_call_id, "research": research_out, "trace": trace,
    }))
}

/// THE ROUTE OF AN ESTIMATE (2026-09-19): which provider row a (tier, engine) choice lands on, what it
/// costs. A `cli` row is a subscription — metered cost 0 — but the engine's API-equivalent price is
/// worth knowing (`est_equiv_usd`), so the table below carries the public per-Mtok prices of the
/// models each engine runs per tier (cli.rs `tier_model`): claude sonnet / opus; codex is one model
/// (the account's) at low / default / high reasoning effort. Metered rows (openrouter) price from their own columns; local rows are free.
pub fn route_estimate(db: &Connection, tier: &str, engine: Option<&str>, tokens_in: i64, tokens_out: i64) -> Value {
    let row = if let Some(e) = engine {
        crate::db_json::query_rows(db, "SELECT * FROM brain.ai_providers WHERE tier = ?1 AND provider = 'cli' AND model_id = ?2 LIMIT 1", &[json!(tier), json!(e)]).ok()
            .and_then(|r| r.as_array().and_then(|a| a.first()).cloned())
            .or_else(|| Some(json!({ "provider": "cli", "model_id": e })))
    } else {
        crate::db_json::query_rows(db, "SELECT * FROM brain.ai_providers WHERE active = 1 AND tier = ?1 AND provider IN ('cli','ollama','openrouter') \
             ORDER BY failover_rank ASC, CASE provider WHEN 'cli' THEN 0 WHEN 'ollama' THEN 1 ELSE 2 END, id ASC LIMIT 1", &[json!(tier)]).ok()
            .and_then(|r| r.as_array().and_then(|a| a.first()).cloned())
    };
    let Some(row) = row else { return json!({ "tier": tier, "provider": Value::Null, "model": Value::Null, "billing": "none", "est_cost_usd": Value::Null, "est_equiv_usd": Value::Null }) };
    let provider = js_string(row.get("provider").unwrap_or(&Value::Null));
    let model_id = js_string(row.get("model_id").unwrap_or(&Value::Null));
    let price = |tin: f64, tout: f64| (tokens_in as f64 / 1e6) * tin + (tokens_out as f64 / 1e6) * tout;
    if provider == "cli" {
        // (model, $in/Mtok, $out/Mtok) per engine × tier — public list prices, the subscription pays them
        let (model, tin, tout): (&str, f64, f64) = match (model_id.as_str(), tier) {
            ("claude-code", "high") => ("opus", 15.0, 75.0),
            ("claude-code", _) => ("sonnet", 3.0, 15.0),
            ("codex", "low") => ("gpt-5 · low effort", 1.25, 10.0),
            ("codex", "high") => ("gpt-5 · high effort", 1.25, 10.0),
            ("codex", _) => ("gpt-5", 1.25, 10.0),
            _ => ("the engine's default", 0.0, 0.0),
        };
        let equiv = if tin > 0.0 { json!(price(tin, tout)) } else { Value::Null };
        return json!({ "tier": tier, "provider": "cli", "engine": model_id, "model": format!("{model_id} · {model}"), "billing": "subscription", "est_cost_usd": 0.0, "est_equiv_usd": equiv,
                       "usd_in_per_mtok": if tin > 0.0 { json!(tin) } else { Value::Null }, "usd_out_per_mtok": if tin > 0.0 { json!(tout) } else { Value::Null } });
    }
    let in_p = nz(row.get("input_usd_per_mtok")).map(|v| js_number(Some(v)));
    let out_p = nz(row.get("output_usd_per_mtok")).map(|v| js_number(Some(v)));
    let metered = provider == "openrouter";
    let cost = match (in_p, out_p) { (Some(a), Some(b)) if metered => json!(price(a, b)), _ if metered => Value::Null, _ => json!(0.0) };
    json!({ "tier": tier, "provider": provider, "engine": Value::Null, "model": model_id, "billing": if metered { "metered" } else { "local" }, "est_cost_usd": cost, "est_equiv_usd": Value::Null,
            "usd_in_per_mtok": if metered { in_p.map(|v| json!(v)).unwrap_or(Value::Null) } else { json!(0.0) }, "usd_out_per_mtok": if metered { out_p.map(|v| json!(v)).unwrap_or(Value::Null) } else { json!(0.0) } })
}

// `ensurePrompt(db)` — get-or-add the prompt template, mid tier, comparing template_md/system_md
// verbatim like the JS. The SYSTEM/TEMPLATE prose itself is NOT re-embedded here (PARITY-DEFERRED
// data, not logic — the actual bytes only matter to a live aiCall dispatch, which is itself
// deferred); this twin always takes the "insert-if-missing" branch with placeholder text, which
// is sufficient to exercise every reachable code path (ensure_faculty precedent in book-run.rs).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    // get-or-add, comparing the prose: a changed SYSTEM/TEMPLATE registers the next version (the
    // placeholder a stale DB still carries is superseded on the first run)
    if let Ok(e) = crate::ai_prompt_find::promptFind(&json!({ "name": "constitution-agent-review" }), db) {
        if e.get("template_md").and_then(Value::as_str) == Some(REVIEW_TEMPLATE) && e.get("system_md").and_then(Value::as_str) == Some(REVIEW_SYSTEM) { return Ok(()); }
    }
    crate::ai_prompt_add::promptAdd(&json!({
        "name": "constitution-agent-review",
        "template_md": REVIEW_TEMPLATE,
        "system_md": REVIEW_SYSTEM,
        "default_tier": "mid",
        "default_temperature": "cold",
        "phase_flow": "single_only",
        "max_output_tokens": 2200,
        "output_fields": [{ "field_name": "review_json", "field_type": "string" }],
    }), db)?;
    Ok(())
}
// THE SPECIALIST'S CONTRACT (2026-09-19 — the JS prose was lost with antiloki-core; rewritten from
// the validator above, which is the contract: summary · theory · ≤4 cards · ≤8 evidence ids drawn
// ONLY from the citable set · ≤2 widgets of the known types · ≤3 actions · an optional lesson).
const REVIEW_SYSTEM: &str = "You are ONE specialist of antiloki's review swarm, reading ONE case (a file or a folder) through ONE lens. Several siblings read the same case through other lenses in parallel; an editor binds every review into a paper afterwards. You write for that editor, not for a human.

Your discipline:
- ONE theory, not ten findings. Name the single most important thing your lens sees in this case and build the review around it. If the case is genuinely fine under your lens, say so (level \"observation\", no theory) — an honest null result is worth more than an invented problem.
- EVIDENCE BY CITATION. Every claim points at an id from the CITABLE set you are given (law ids, axis names, file paths, or the web sources listed). Cite nothing else — an uncited id is dropped by the validator and a theory with no valid evidence is demoted to an unsupported hypothesis.
- MEASURED FIRST. The verdict, the laws, the recorded violations, the field (the neighborhood) and the population baseline are deterministic facts computed by antiloki; read the source to explain them, never to contradict them without saying so explicitly.
- DRAW-FIRST. Prefer a widget over prose when a structure carries the point (a repair order, a before/after, a flow, a root-cause tree).
- BE CONCRETE. Names, lines, symbols, numbers. No generic advice.

Answer with ONE JSON object and nothing else — no prose around it, no markdown fence:
{\"summary\": {\"level\": \"observation|finding|insight|discovery|revelation\", \"title\": \"<≤120 chars>\", \"headline\": \"<one sentence, ≤200 chars>\"},
 \"theory\": {\"name\": \"<the one theory>\", \"explanation\": \"<≤700 chars>\", \"root_cause\": \"<≤300>\", \"engineering_story\": \"<how it came to be, ≤500>\", \"if_nothing_changes\": \"<≤300>\", \"confidence\": <0..1>} | null,
 \"cards\": [{\"type\": \"discovery|root-cause|tradeoff|repair|risk|question\", \"severity\": \"info|warn|error\", \"title\": \"<≤120>\", \"body\": \"<≤500>\", \"confidence\": <0..1>}],   (≤4)
 \"evidence\": [{\"kind\": \"law|axis|file|web\", \"id\": \"<an id from CITABLE, verbatim>\"}],   (≤8)
 \"widgets\": [{\"type\": \"graph-focus|before-after|repair-order|root-cause-tree|timeline|flow|balance|layers\", \"title\": \"<≤120>\", \"payload\": {...}}],   (≤2; payload: repair-order {steps:[{title,why}]} · before-after {before,after} · flow {steps:[string]} · root-cause-tree {root, causes:[{cause, effects:[string]}]} · timeline {events:[{when,what}]} · balance {left:{label,items:[]},right:{label,items:[]}} · layers {layers:[{name,items:[]}]} · graph-focus {center, edges:[{to,why}]})
 \"actions\": [{\"title\": \"<≤120>\", \"strategy\": \"<what to change, concretely, ≤300>\", \"impact\": \"small|medium|large\", \"effort\": \"xs|s|m|l|xl\"}],   (≤3, best first)
 \"expected_pushback\": \"<the strongest objection a maintainer would raise, ≤200>\",
 \"lesson\": {\"concept\": \"<a named engineering concept this case teaches, ≤140>\", \"sample\": \"<a ≤700-char code sample illustrating it>\", \"caption\": \"<≤220>\"} | null}";

const REVIEW_TEMPLATE: &str = "YOUR LENS: {{lens}}

THE CASE: {{path}}

THE VERDICT (measured by antiloki):
{{verdict}}

THE LAWS YOUR LENS ENFORCES:
{{laws}}

RECORDED VIOLATIONS (with evidence, at exact lines):
{{violations}}

THE FIELD (the 1-ring neighborhood — what this case imports and who imports it, with the symbols each edge carries):
{{field}}

THE POPULATION BASELINE (the same laws counted across the whole snapshot — what 'normal' looks like here):
{{baseline}}

ADDITIONAL CONTEXT THE OPERATOR ATTACHED:
{{sources}}

WEB RESEARCH (scraped before you ran — real pages; treat as UNVERIFIED unless the source itself shows it):
{{research}}

CITABLE IDS (the only ids `evidence` may reference, verbatim): {{citable}}

THE SOURCE (truncated at 5k chars):
{{source}}

Write the review JSON now — one theory, cited evidence, concrete actions.";

#[cfg(test)]
mod tests {
    use super::*;

    // ── extractJson ──────────────────────────────────────────────────────────────────────────
    #[test]
    fn extract_json_plain_object() {
        let r = extractJson(&json!(r#"{"a":1,"b":"x"}"#), 0).unwrap();
        assert_eq!(r, json!({"a":1,"b":"x"}));
    }

    #[test]
    fn extract_json_with_preamble_and_trailing_prose() {
        let r = extractJson(&json!(r#"sure, here it is: {"a":1} — hope that helps"#), 0).unwrap();
        assert_eq!(r, json!({"a":1}));
    }

    #[test]
    fn extract_json_double_wrapped_review_json() {
        let raw = r#"{"review_json": "{\"summary\":{\"title\":\"t\"}}"}"#;
        let r = extractJson(&json!(raw), 0).unwrap();
        assert_eq!(r, json!({"summary":{"title":"t"}}));
    }

    #[test]
    fn extract_json_truncated_mid_array_salvages_closed_prefix() {
        let raw = r#"{"summary":{"title":"t"},"cards":[{"title":"a"},{"title":"b"#;
        let r = extractJson(&json!(raw), 0).unwrap();
        assert_eq!(r["summary"]["title"], json!("t"));
        assert_eq!(r["cards"][0]["title"], json!("a"));
        // real JS extractJson (verified via node): the mid-string cut lands right after
        // the second card's `"title"` key closes, so tryClose strips that dangling key
        // and closes the still-open card object → a second, empty card survives (`{}`),
        // it is not dropped entirely.
        assert_eq!(r["cards"], json!([{"title": "a"}, {}]));
    }

    #[test]
    fn extract_json_no_brace_returns_none() {
        assert!(extractJson(&json!("no json here"), 0).is_none());
    }

    #[test]
    fn extract_json_unparseable_garbage_returns_none() {
        assert!(extractJson(&json!("{not: valid, at: all"), 0).is_none());
    }

    // ── validate_review ──────────────────────────────────────────────────────────────────────
    fn citable_set(items: &[&str]) -> HashSet<String> { items.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn validate_review_none_body_is_dropped() {
        let (review, validation) = validate_review(None, &citable_set(&[]));
        assert!(review.is_null());
        assert_eq!(validation["parsed"], json!(false));
    }

    #[test]
    fn validate_review_object_without_summary_or_theory_is_dropped() {
        let body = json!({ "cards": [] });
        let (review, validation) = validate_review(Some(&body), &citable_set(&[]));
        assert!(review.is_null());
        assert_eq!(validation["parsed"], json!(false));
    }

    #[test]
    fn validate_review_uncited_evidence_is_dropped_and_counted() {
        let body = json!({
            "summary": { "title": "t" },
            "theory": { "name": "n", "explanation": "e", "confidence": 0.8 },
            "evidence": [{ "kind": "law", "id": "LAW-A" }, { "kind": "law", "id": "NOT-CITABLE" }],
        });
        let (review, validation) = validate_review(Some(&body), &citable_set(&["LAW-A"]));
        assert_eq!(review["evidence"].as_array().unwrap().len(), 1);
        assert_eq!(review["evidence"][0]["id"], json!("LAW-A"));
        assert_eq!(validation["uncited_dropped"], json!(1));
        assert_eq!(review["theory"]["unsupported"], json!(false));
    }

    #[test]
    fn validate_review_zero_valid_citations_demotes_to_unsupported_observation() {
        let body = json!({
            "summary": { "title": "t", "level": "revelation" },
            "theory": { "name": "n" },
            "evidence": [{ "kind": "law", "id": "NOT-CITABLE" }],
        });
        let (review, _v) = validate_review(Some(&body), &citable_set(&["LAW-A"]));
        assert_eq!(review["theory"]["unsupported"], json!(true));
        assert_eq!(review["summary"]["level"], json!("observation"));
    }

    #[test]
    fn validate_review_unknown_widget_type_converts_to_a_card() {
        let body = json!({
            "summary": { "title": "t" },
            "widgets": [{ "type": "pie-chart", "title": "bad" }],
        });
        let (review, validation) = validate_review(Some(&body), &citable_set(&[]));
        assert_eq!(review["widgets"].as_array().unwrap().len(), 0);
        assert_eq!(validation["widgets_converted"], json!(1));
        let cards = review["cards"].as_array().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["type"], json!("question"));
        assert!(cards[0]["body"].as_str().unwrap().contains("pie-chart"));
    }

    #[test]
    fn validate_review_known_widget_passes_through_capped_to_two() {
        let body = json!({
            "summary": { "title": "t" },
            "widgets": [
                { "type": "flow", "title": "f1", "payload": { "steps": ["a"] } },
                { "type": "timeline", "title": "f2" },
                { "type": "layers", "title": "f3" },
            ],
        });
        let (review, _v) = validate_review(Some(&body), &citable_set(&[]));
        assert_eq!(review["widgets"].as_array().unwrap().len(), 2);
        assert_eq!(review["widgets"][0]["type"], json!("flow"));
    }

    #[test]
    fn validate_review_confidence_is_clamped_0_to_1() {
        let body = json!({ "summary": { "title": "t" }, "cards": [{ "title": "c", "confidence": 5.0 }] });
        let (review, _v) = validate_review(Some(&body), &citable_set(&[]));
        assert_eq!(review["cards"][0]["confidence"], json!(1.0));
    }

    #[test]
    fn validate_review_caps_cards_actions_evidence() {
        let cards: Vec<Value> = (0..10).map(|i| json!({ "title": format!("c{i}") })).collect();
        let actions: Vec<Value> = (0..10).map(|i| json!({ "title": format!("a{i}") })).collect();
        let body = json!({ "summary": { "title": "t" }, "cards": cards, "actions": actions });
        let (review, _v) = validate_review(Some(&body), &citable_set(&[]));
        assert_eq!(review["cards"].as_array().unwrap().len(), 4);
        assert_eq!(review["actions"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn validate_review_lesson_kept_only_when_concept_or_sample_present() {
        let body = json!({ "summary": { "title": "t" }, "lesson": { "concept": "", "sample": "" } });
        let (review, _v) = validate_review(Some(&body), &citable_set(&[]));
        assert!(review["lesson"].is_null());
        let body2 = json!({ "summary": { "title": "t" }, "lesson": { "concept": "Interface Segregation", "sample": "x" } });
        let (review2, _v2) = validate_review(Some(&body2), &citable_set(&[]));
        assert_eq!(review2["lesson"]["concept"], json!("Interface Segregation"));
    }

    // ── constitutionAgentRun (deterministic gates) ──────────────────────────────────────────
    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS code;").unwrap();
        c.execute_batch(
            "CREATE TABLE laws.review_laws (key TEXT PRIMARY KEY, title TEXT, rule_md TEXT, detector TEXT, config_json TEXT, scope TEXT, applies_json TEXT, framework TEXT, status TEXT NOT NULL DEFAULT 'ratified', enforcement TEXT NOT NULL DEFAULT 'score', category TEXT);
             CREATE TABLE judgments.review_snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, status TEXT NOT NULL DEFAULT 'done', created_at TEXT, engineering_version TEXT, constitution_hash TEXT);
             CREATE TABLE judgments.review_scores (id INTEGER PRIMARY KEY AUTOINCREMENT, snapshot_id INTEGER, unit_kind TEXT, path TEXT, health REAL, score REAL, axes_json TEXT, files INTEGER);
             CREATE TABLE judgments.review_violations (id INTEGER PRIMARY KEY AUTOINCREMENT, snapshot_id INTEGER, path TEXT, law_key TEXT, law_scope TEXT, weight REAL, severity TEXT, confidence REAL, line INTEGER, symbol TEXT, evidence TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_providers (id INTEGER PRIMARY KEY, active INTEGER, tier TEXT, provider TEXT NOT NULL DEFAULT 'openrouter', model_id TEXT, failover_rank INTEGER NOT NULL DEFAULT 0, input_usd_per_mtok REAL, output_usd_per_mtok REAL);
             CREATE TABLE code.code_edges (id INTEGER PRIMARY KEY);
             CREATE TABLE code.code_nodes (id INTEGER PRIMARY KEY, kind TEXT);",
        ).unwrap();
        c
    }

    fn seed_snapshot_with_file(c: &Connection, path: &str) -> i64 {
        c.execute("INSERT INTO judgments.review_snapshots (status) VALUES ('done')", []).unwrap();
        let sid = c.last_insert_rowid();
        c.execute("INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, health, score, axes_json) VALUES (?, 'file', ?, 60, 60, '{}')", rusqlite::params![sid, path]).unwrap();
        sid
    }

    #[test]
    fn requires_agent_id_and_path() {
        let c = setup();
        assert!(constitutionAgentRun(&json!({}), &c).unwrap_err().contains("agent_id is required"));
        assert!(constitutionAgentRun(&json!({ "agent_id": "custom" }), &c).unwrap_err().contains("path is required"));
    }

    #[test]
    fn custom_agent_requires_instruction() {
        let c = setup();
        let r = constitutionAgentRun(&json!({ "agent_id": "custom", "path": "a.js" }), &c).unwrap_err();
        assert!(r.contains("custom_instruction"));
    }

    #[test]
    fn unknown_registry_agent_returns_ok_false() {
        let c = setup();
        let r = constitutionAgentRun(&json!({ "agent_id": "nope", "path": "a.js" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("no such specialist"));
    }

    #[test]
    fn path_outside_snapshot_and_not_on_disk_returns_ok_false() {
        let c = setup();
        let r = constitutionAgentRun(&json!({ "agent_id": "custom", "custom_instruction": "review this", "path": "missing.js" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("outside the latest snapshot and not a file"));
        // an unjudged file that exists on disk is reviewed from the source: the estimate says so in its verdict
        let e = constitutionAgentRun(&json!({ "agent_id": "custom", "custom_instruction": "review this", "path": "Cargo.toml", "estimate": true, "research": false }), &c).unwrap();
        assert_eq!(e["ok"], json!(true));
    }

    #[test]
    fn estimate_mode_spends_no_ai_and_returns_token_estimate() {
        let c = setup();
        seed_snapshot_with_file(&c, "src/a.js");
        c.execute("INSERT INTO brain.ai_providers (active, tier, model_id, input_usd_per_mtok, output_usd_per_mtok) VALUES (1,'mid','some-model',1.0,5.0)", []).unwrap();
        let r = constitutionAgentRun(&json!({
            "agent_id": "custom", "custom_instruction": "review this", "path": "src/a.js", "estimate": true, "research": false,
        }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["estimate"], json!(true));
        assert!(r["est_tokens_in"].as_i64().unwrap() > 0);
        assert_eq!(r["model"], json!("some-model"));
        assert_eq!(r["billing"], json!("metered"));
        assert!(r["est_cost_usd"].as_f64().unwrap() > 0.0);
        // the run-on choice: a cli engine at a tier prices as a subscription with an API-equivalent
        c.execute("INSERT INTO brain.ai_providers (active, tier, provider, model_id) VALUES (1,'high','cli','claude-code')", []).unwrap();
        let r2 = constitutionAgentRun(&json!({
            "agent_id": "custom", "custom_instruction": "review this", "path": "src/a.js", "estimate": true, "research": false, "tier": "high", "engine": "claude-code",
        }), &c).unwrap();
        assert_eq!((r2["billing"].as_str(), r2["model"].as_str(), r2["tier"].as_str()), (Some("subscription"), Some("claude-code · opus"), Some("high")));
        assert_eq!(r2["est_cost_usd"], json!(0.0));
        assert!(r2["est_equiv_usd"].as_f64().unwrap() > r["est_cost_usd"].as_f64().unwrap(), "opus list price beats a $1/$5 model");
        // an engine with no row still estimates (the operator's explicit choice)
        let r3 = constitutionAgentRun(&json!({ "agent_id": "custom", "custom_instruction": "x", "path": "src/a.js", "estimate": true, "research": false, "tier": "low", "engine": "codex" }), &c).unwrap();
        assert_eq!(r3["model"], json!("codex · gpt-5 · low effort"));
    }

    #[test]
    fn full_run_reaches_the_parity_deferred_aicall_boundary_after_three_attempts() {
        let c = setup();
        seed_snapshot_with_file(&c, "src/a.js");
        let r = constitutionAgentRun(&json!({
            "agent_id": "custom", "custom_instruction": "review this", "path": "src/a.js", "research": false,
        }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["attempts"], json!(3));
        assert_eq!(r["dropped"], json!(1));
        assert!(r["review"].is_null());
        assert!(r["parse_excerpt"].as_str().unwrap().contains("ai-call"));
        // the prompt template WAS ensured (ensure_prompt ran) before the deferred dispatch
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-agent-review'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn default_research_setting_still_reaches_deferred_boundary_via_fallback_text() {
        // proves the constitution-research stub's non-fatal fallback lets the run proceed even
        // when research isn't explicitly disabled (the JS default).
        let c = setup();
        seed_snapshot_with_file(&c, "src/a.js");
        let r = constitutionAgentRun(&json!({
            "agent_id": "custom", "custom_instruction": "review this", "path": "src/a.js",
        }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["attempts"], json!(3));
    }
}
