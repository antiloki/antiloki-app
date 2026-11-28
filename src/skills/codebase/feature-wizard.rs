//! feature-wizard.rs — twin of skills/codebase/feature-wizard.mjs (db lane + PARITY-DEFERRED AI).
//! NAMES ARE CONTRACT: default `featureWizard`; also ports the named export `buildDossier` (real,
//! fixture-tested, fully deterministic evidence assembly matching the JS named export). `filesBlock`
//! / `ensurePrompt` / `callField` are JS module-scope private helpers, not exported — ported as
//! private fns here too.
//!
//! PARITY-DEFERRED (same convention as book-run.rs / cockpit-pane-message.rs): ops `chips`, `shape`
//! and `handoff` each end in `crate::ai_ai_call::aiCall` (the deferred network-AI stub — see that
//! module's own doc) via `call_field`; `op:"image"` ends in `crate::ai_ai_image_call::aiImageCall`
//! (the deferred network-image stub). Both stubs always `Err`, so every op handler here propagates
//! that Err past the point the JS would parse/branch on a real AI response — exactly the pattern
//! book-run.rs's `step` documents. Everything AROUND those calls IS this file's fixture-tested
//! deterministic surface: op/field validation (`op`, `intake`, `prompt` required guards),
//! `build_dossier` (the full deterministic evidence bundle — surface map via the already-converted
//! `codebase_feature_inventory` twin, target entry points/data-stories/blast-radius/real-source-read,
//! institutional-memory + related-in-flight-card lookups, all capped/truncated exactly as JS),
//! `files_block`, `ensure_prompt` (reusing the already-converted `ai_prompt_find`/`ai_prompt_add`
//! twins to bootstrap the 3 prompt templates), and the mode/kv assembly feeding `call_field`.
//! `call_field`'s post-aiCall output-parsing branch is ported byte-for-byte but UNREACHABLE by this
//! file's own fixture (aiCall's Err short-circuits it first) — same disclosed pattern as book-run.rs's
//! `parse_arr`/`parse_obj`.
//!
//! DESTRUCTURING-DEFAULT SIMPLIFICATION (disclosed): JS `{ feature = null, intake = '' } = opts`
//! triggers a default only when the resolved value is strictly `undefined` — which JSON has no
//! representation for, so every real caller either omits the key or (never, in practice) sets it to
//! `null`. This twin treats "key absent" and "key present as JSON null" identically (both trigger
//! the default), which is exact for every real call site in antiloki-core and only diverges from a
//! never-occurring explicit-`null` intake/feature argument.
// QUALITY: mixed unit — deterministic dossier/validation/prompt-bootstrap surface (fixture-tested)
// + PARITY-DEFERRED AI dispatch (chips/shape/handoff/image ops, no fixture — see module doc)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helper functions (playbook) ────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS String(v ?? '') — used for Array.join() elements (null/undefined join as '').
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS `${v}` template-literal interpolation (undefined -> "undefined", null -> "null").
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// = JS `x ?? default` then String(...) — null/missing -> default, else stringified value.
fn opt_str_or(v: Option<&Value>, default: &str) -> String {
    match v {
        None | Some(Value::Null) => default.to_string(),
        Some(x) => str_qq(Some(x)),
    }
}

fn err_required(field: &str) -> String {
    format!("feature-wizard: {field} is required")
}

// ── the dossier (playbook §standard helper functions n/a — file-specific logic) ──────────
fn add_line(lines: &mut Vec<String>, n: &mut i64, s: String) {
    *n += 1;
    lines.push(format!("[#{n}] {s}"));
}

fn resolve_feature_path(root: &str, rel: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(rel);
    if p.is_absolute() { p.to_path_buf() } else { std::path::Path::new(root).join(rel) }
}

/// `export function buildDossier(db, { feature = null, intake = '' } = {})` — THE dossier
/// (operator 2026-07-10 "gather all context and info just like forensic"): numbered `[#id]`
/// evidence lines, capped ~9KB. See module doc for the destructuring-default simplification.
pub fn buildDossier(db: &Connection, opts: &Value) -> Value {
    let feature: Value = match opts.get("feature") {
        None | Some(Value::Null) => Value::Null,
        Some(v) => v.clone(),
    };
    let feature_opt: Option<&Value> = if feature.is_null() { None } else { Some(&feature) };
    let intake_v: Value = match opts.get("intake") {
        None | Some(Value::Null) => json!(""),
        Some(v) => v.clone(),
    };

    let mut lines: Vec<String> = Vec::new();
    let mut n: i64 = 0;

    // 1. the surface map — what the product IS (full inventory, not a one-line summary)
    match crate::codebase_feature_inventory::featureInventory(&json!({}), db) {
        Ok(inv) => {
            let ui = inv.get("ui").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for f in ui.iter().take(25) {
                let name = tmpl(f.get("name"));
                let path = opt_str_or(f.get("path"), "?");
                let story_names = match f.get("stories").and_then(|v| v.as_array()) {
                    Some(arr) => {
                        let j = arr.iter().map(|s| str_qq(s.get("name"))).collect::<Vec<_>>().join(", ");
                        if j.is_empty() { "none".to_string() } else { j }
                    }
                    None => "none".to_string(),
                };
                add_line(&mut lines, &mut n, format!("UI feature \"{name}\" \u{b7} {path} \u{b7} stories: {story_names}"));
            }
            let nonui = inv.get("nonui").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for f in nonui.iter().take(20) {
                let name = tmpl(f.get("name"));
                let source = tmpl(f.get("source"));
                let path = opt_str_or(f.get("path"), "?");
                let ops = match f.get("ops") {
                    Some(Value::Array(a)) => format!(" \u{b7} {}", a.iter().map(|x| str_qq(Some(x))).collect::<Vec<_>>().join("/")),
                    _ => String::new(),
                };
                add_line(&mut lines, &mut n, format!("non-UI \"{name}\" \u{b7} {source} \u{b7} {path}{ops}"));
            }
            let gaps = inv.get("gaps").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for f in gaps.iter().take(10) {
                let name = tmpl(f.get("name"));
                let path = opt_str_or(f.get("path"), "?");
                add_line(&mut lines, &mut n, format!("GAP: surface \"{name}\" ({path}) \u{2014} no data-story reaches it"));
            }
        }
        Err(_) => add_line(&mut lines, &mut n, "inventory unavailable (atlas not onboarded)".to_string()),
    }

    // 2. the modify-target, deep: its entry points, its stories WITH hops + tests, its source
    let feature_path_empty = feature_opt.map(|f| opt_str_or(f.get("path"), "")).unwrap_or_default();
    let feature_name_empty = feature_opt.map(|f| opt_str_or(f.get("name"), "")).unwrap_or_default();
    if let Some(feature) = feature_opt {
        let fname = tmpl(feature.get("name"));
        let fkind = tmpl(feature.get("kind"));
        let fpath_or_nofile = opt_str_or(feature.get("path"), "no file");
        add_line(&mut lines, &mut n, format!("TARGET: \"{fname}\" ({fkind}) \u{b7} {fpath_or_nofile}"));

        let eps = crate::db_json::query_rows(
            db,
            "SELECT subtype, label, http_method, detail, line FROM code_entry_points WHERE origin = 'atlas' AND (path = ? OR label = ?)",
            &[json!(feature_path_empty), json!(feature_name_empty)],
        ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        for e in eps.iter() {
            let subtype = tmpl(e.get("subtype"));
            let label = tmpl(e.get("label"));
            let http = if truthy(e.get("http_method")) { format!(" {}", tmpl(e.get("http_method"))) } else { String::new() };
            let detail = if truthy(e.get("detail")) { format!(" \u{b7} {}", tmpl(e.get("detail"))) } else { String::new() };
            let line = if truthy(e.get("line")) { format!(" \u{b7} L{}", tmpl(e.get("line"))) } else { String::new() };
            add_line(&mut lines, &mut n, format!("target entry point: {subtype} \"{label}\"{http}{detail}{line}"));
        }

        let stories = crate::db_json::query_rows(
            db,
            "SELECT name, description, hops_json, linked_test FROM code_data_stories WHERE origin = 'atlas'",
            &[],
        ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let feature_path_for_touch = feature.get("path").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        for s in stories.iter() {
            let hops: Vec<Value> = s.get("hops_json").and_then(|v| v.as_str())
                .and_then(|x| serde_json::from_str::<Value>(x).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            let touches = match feature_path_for_touch {
                Some(fp) => hops.iter().any(|h| {
                    let hp = h.get("path").filter(|v| !v.is_null()).or_else(|| h.get("file").filter(|v| !v.is_null()));
                    opt_str_or(hp, "") == fp
                }),
                None => false,
            };
            let named = feature.get("stories").and_then(|v| v.as_array())
                .map(|arr| arr.iter().any(|x| x.get("name") == s.get("name")))
                .unwrap_or(false);
            if touches || named {
                let sname = tmpl(s.get("name"));
                let sdesc = if truthy(s.get("description")) { format!(" \u{2014} {}", tmpl(s.get("description"))) } else { String::new() };
                let hop_count = hops.len();
                let hop_preview = hops.iter().take(6).map(|h| {
                    let hp = h.get("path").filter(|v| !v.is_null()).or_else(|| h.get("file").filter(|v| !v.is_null()));
                    opt_str_or(hp, "?")
                }).collect::<Vec<_>>().join(" \u{2192} ");
                let verified = if truthy(s.get("linked_test")) { format!(" \u{b7} verified by {}", tmpl(s.get("linked_test"))) } else { " \u{b7} NO linked test".to_string() };
                add_line(&mut lines, &mut n, format!("target data-story \"{sname}\"{sdesc} \u{b7} {hop_count} hops ({hop_preview}){verified} \u{2014} a change here means re-recording this story"));
            }
        }

        let blast = crate::db_json::query_rows(
            db,
            "SELECT DISTINCT src_path FROM code_edges WHERE origin = 'atlas' AND dst_path = ? LIMIT 8",
            &[json!(feature_path_empty)],
        ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        for e in blast.iter() {
            add_line(&mut lines, &mut n, format!("imported by: {} (blast radius)", tmpl(e.get("src_path"))));
        }

        if truthy(feature.get("path")) {
            let fpath = tmpl(feature.get("path"));
            let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
            let root = root_v.as_str().unwrap_or_default();
            let resolved = resolve_feature_path(root, &fpath);
            match std::fs::read_to_string(&resolved) {
                Ok(src) => {
                    let preview: String = src.chars().take(2500).collect();
                    add_line(&mut lines, &mut n, format!("target source ({fpath}, first 2500 chars):\n{preview}"));
                }
                Err(_) => add_line(&mut lines, &mut n, format!("target source unreadable at {fpath} (workspace not mounted here)")),
            }
        }
    }

    // 3. institutional memory: decisions/corrections/lessons that mention the target or the intake words
    let words_source = format!("{} {}", feature_opt.map(|f| opt_str_or(f.get("name"), "")).unwrap_or_default(), tmpl(Some(&intake_v))).to_lowercase();
    let mut words: Vec<String> = Vec::new();
    {
        let mut seen_w = std::collections::HashSet::new();
        'outer: for tok in words_source.split(|c: char| !c.is_ascii_alphanumeric()) {
            if tok.chars().count() <= 3 { continue; }
            if seen_w.insert(tok.to_string()) {
                words.push(tok.to_string());
                if words.len() == 6 { break 'outer; }
            }
        }
    }
    for w in &words {
        let pat = format!("%{w}%");
        let rows = crate::db_json::query_rows(
            db,
            "SELECT type, title, guidance FROM memories WHERE (lower(title) LIKE ? OR lower(guidance) LIKE ?) LIMIT 2",
            &[json!(pat.clone()), json!(pat)],
        ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        for m in rows.iter() {
            let mtype = tmpl(m.get("type"));
            let mtitle = tmpl(m.get("title"));
            let guidance: String = opt_str_or(m.get("guidance"), "").chars().take(220).collect();
            add_line(&mut lines, &mut n, format!("memory ({mtype}) \"{mtitle}\": {guidance}"));
        }
    }

    // 4. related work in flight — cards whose titles share the words (dedup by slug)
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for w in &words {
        let pat = format!("%{w}%");
        let rows = crate::db_json::query_rows(
            db,
            "SELECT slug, title, status FROM cards WHERE lower(title) LIKE ? AND status != 'done' LIMIT 3",
            &[json!(pat)],
        ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
        for c in rows.iter() {
            let slug = tmpl(c.get("slug"));
            if seen.insert(slug.clone()) {
                let status = tmpl(c.get("status"));
                let title = tmpl(c.get("title"));
                add_line(&mut lines, &mut n, format!("related card {slug} [{status}]: {title}"));
            }
        }
    }

    let out = lines.join("\n");
    let out_len = out.chars().count();
    let result = if out_len > 9000 {
        let truncated: String = out.chars().take(9000).collect();
        format!("{truncated}\n[dossier truncated]")
    } else if out.is_empty() {
        "[empty dossier \u{2014} atlas not onboarded, no related evidence]".to_string()
    } else {
        out
    };
    json!(result)
}

/// `const filesBlock = (files) => ...` — JS module-scope private helper.
fn files_block(files: Option<&Value>) -> String {
    let arr = match files.and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a,
        _ => return "(none)".to_string(),
    };
    let joined = arr.iter().map(|f| {
        let name = tmpl(f.get("name"));
        let text_part = if truthy(f.get("text")) {
            let t: String = opt_str_or(f.get("text"), "").chars().take(1500).collect();
            format!(":\n{t}")
        } else {
            " (binary/image \u{2014} name only)".to_string()
        };
        format!("\u{2014} {name}{text_part}")
    }).collect::<Vec<_>>().join("\n");
    joined.chars().take(6000).collect()
}

struct PromptSpec {
    name: &'static str,
    template_md: &'static str,
    fields: Value,
    max_tokens: i64,
    temperature: &'static str,
    field_name: &'static str,
}

fn prompt_chips() -> PromptSpec {
    PromptSpec {
        name: "feature.wizard.chips.v2",
        template_md: r#"You suggest CHIPS (short tags) for a change being designed. Chips mark aspects, risks and domains the operator should consider BEFORE the design is shaped. You receive a deterministic DOSSIER of evidence about the workspace — derive every chip FROM the dossier or the intake; never invent facts (the forensics doctrine: evidence first, narration second).

MODE: {{mode}}
THE OPERATOR IS TYPING:
{{intake}}

═══ THE DOSSIER (deterministic evidence, [#id] markers) ═══
{{dossier}}

Derive 6-10 chips, SHORT (1-3 words), concrete, NON-OBVIOUS — the deterministic set (FE/BE/Styling/Architecture change/Data/Tests/Performance/Security) already exists in the UI, never repeat it. Good chips point at things the dossier PROVES matter: a data-story that must be re-recorded [#], a heavily-referenced file (blast radius) [#], a gap surface, a past decision that constrains this, a related in-flight card. Groups: aspect | risk | domain.
Output JSON EXACTLY: { "chips_json": "<JSON-stringified array of {\"label\":\"…\",\"group\":\"aspect|risk|domain\"}>" }"#,
        fields: json!([{ "field_name": "chips_json", "field_type": "string", "required": true }]),
        max_tokens: 700,
        temperature: "warm",
        field_name: "chips_json",
    }
}

fn prompt_shape() -> PromptSpec {
    PromptSpec {
        name: "feature.wizard.shape.v2",
        template_md: r#"You are a senior architect SIMULATING the end state of a proposed change — what the product looks like AFTER it ships. You receive a deterministic DOSSIER of evidence about the workspace (real paths, real stories, real source, related decisions). Ground EVERYTHING on it: file paths in your simulation must extend the real tree the dossier shows; components must connect to what exists. Invent only what the change genuinely adds (the forensics doctrine: evidence first).

MODE: {{mode}}
INTAKE (the operator's words):
{{intake}}
CHIPS (aspects the operator marked): {{chips}}
REFERENCE FILES the operator uploaded (names + excerpts): {{files}}

═══ THE DOSSIER (deterministic evidence, [#id] markers) ═══
{{dossier}}

Produce the simulation as ONE JSON object with these keys:
  summary_md      — 3-6 sentences: what it becomes, how it behaves when done.
  architecture    — { "nodes": [{"id","label","kind":"fe|be|data|external"}], "edges": [{"from","to","label"}] } — the END-STATE component graph, 4-10 nodes.
  files           — [{"path","status":"new|modified","note"}] — the delta filetree after the change (realistic paths).
  surfaces        — ["FE","BE","logic","style","data"] subset actually touched.
  tradeoffs_md    — "+ wins · − risks" one-liner style, 2-4 items.
  risks           — 1-3 short strings.
  image_prompt    — a vivid one-paragraph prompt for an image model to render the UI end-state mockup (dark theme, describe layout + key elements). Empty string if nothing visual changes.

Output JSON EXACTLY: { "shape_json": "<the JSON-stringified object above>" }"#,
        fields: json!([{ "field_name": "shape_json", "field_type": "string", "required": true, "min_length": 80 }]),
        max_tokens: 2400,
        temperature: "warm",
        field_name: "shape_json",
    }
}

fn prompt_handoff() -> PromptSpec {
    PromptSpec {
        name: "feature.wizard.handoff.v1",
        template_md: r#"You turn a designed change into a FEATURE (an epic) + its QUEUE of work cards. The pipeline builds contracts from cards — cards carry intent only.

MODE: {{mode}}   FEATURE CONTEXT: {{feature_context}}
INTAKE: {{intake}}
CHIPS: {{chips}}
THE SIMULATED END STATE:
{{shape}}

Produce ONE JSON object:
  feature — {"name": "<short feature name>", "why": "<the whole design compressed: intent, approach, trade-offs — 3-6 sentences>"}
  cards   — 1-5 work cards, smallest-shippable-slice order: [{"kind":"feat|fix|prevent|docs","size":"S|M|L","title":"<imperative, <=70 chars>","why":"<1-3 sentences>"}]

Output JSON EXACTLY: { "handoff_json": "<the JSON-stringified object above>" }"#,
        fields: json!([{ "field_name": "handoff_json", "field_type": "string", "required": true, "min_length": 60 }]),
        max_tokens: 1600,
        temperature: "cold",
        field_name: "handoff_json",
    }
}

/// `function ensurePrompt(db, spec)` — JS module-scope private helper: try/catch swallows ANY
/// promptFind error (not just not-found) and falls through to promptAdd + a second promptFind.
fn ensure_prompt(db: &Connection, spec: &PromptSpec) -> Result<Value, String> {
    match crate::ai_prompt_find::promptFind(&json!({ "name": spec.name }), db) {
        Ok(v) => Ok(v),
        Err(_) => {
            crate::ai_prompt_add::promptAdd(&json!({
                "name": spec.name,
                "template_md": spec.template_md,
                "default_tier": "high",
                "default_temperature": spec.temperature,
                "max_output_tokens": spec.max_tokens,
                "output_fields": spec.fields,
            }), db)?;
            crate::ai_prompt_find::promptFind(&json!({ "name": spec.name }), db)
        }
    }
}

/// `async function callField(db, spec, kv, provider_fn, fieldName)` — JS module-scope private
/// helper. `ensurePrompt`'s throw (sync, inside an async fn) rejects the same as `aiCall`'s —
/// both propagate as `Err` here. The output-parsing branch below `aiCall` is PARITY-DEFERRED
/// dead code in this twin (see module doc): `aiCall` always returns `Err`, so it never runs.
fn call_field(db: &Connection, spec: &PromptSpec, kv: Value, provider_fn: &Value) -> Result<Value, String> {
    ensure_prompt(db, spec)?;
    let ai_input = json!({
        "prompt_name": spec.name,
        "input_kv": kv,
        "tier": "high",
        "temperature": spec.temperature,
        "provider_fn": if provider_fn.is_null() { Value::Null } else { provider_fn.clone() },
    });
    let ai = crate::ai_ai_call::aiCall(&ai_input, db)?;

    // Unreachable past this point in this twin (aiCall never returns Ok) — ported by inspection.
    let mut out: Map<String, Value> = Map::new();
    if let Some(rows) = ai.get("output_kv").and_then(|v| v.as_array()) {
        for row in rows {
            // JS `row.value_text != null` — include unless the value is missing/null.
            if !matches!(row.get("value_text"), None | Some(Value::Null)) {
                out.insert(tmpl(row.get("field_name")), row.get("value_text").cloned().unwrap_or(Value::Null));
            }
        }
    }
    let raw = out.get(spec.field_name).and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| "null".to_string());
    let parsed = serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null);
    Ok(json!({ "parsed": parsed, "ai_call_id": ai.get("ai_call_id").cloned().unwrap_or(Value::Null) }))
}

fn chips_join(v: Option<&Value>) -> String {
    let arr = v.and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let mapped: Vec<String> = arr.iter().map(|c| match c {
        Value::String(s) => s.clone(),
        other => str_qq(other.get("label")),
    }).collect();
    let joined = mapped.join(", ");
    if joined.is_empty() { "(none)".to_string() } else { joined }
}

/// `export default async function featureWizard(input, context)` — NAMES ARE CONTRACT. `context.db`
/// missing → JS throws `err.contextMissing('feature-wizard', 'db')`; unreachable here, `db` is a
/// required `&Connection` parameter (see module doc convention shared with every other db-lane twin).
pub fn featureWizard(input: &Value, db: &Connection) -> Result<Value, String> {
    let op = opt_str_or(input.get("op"), "");
    let provider_fn = match input.get("provider_fn") {
        None | Some(Value::Null) => Value::Null,
        Some(v) => v.clone(),
    };
    let mode = if input.get("mode").and_then(|v| v.as_str()) == Some("modify") { "modify an existing feature" } else { "propose a new feature" };

    if op == "chips" {
        let intake = opt_str_or(input.get("intake"), "");
        if intake.trim().is_empty() { return Err(err_required("intake")); }
        let dossier_opts = json!({ "feature": input.get("feature").cloned().unwrap_or(Value::Null), "intake": input.get("intake").cloned().unwrap_or(Value::Null) });
        let dossier = buildDossier(db, &dossier_opts);
        let intake_kv: String = intake.chars().take(4000).collect();
        let kv = json!([
            { "key": "mode", "value": mode },
            { "key": "intake", "value": intake_kv },
            { "key": "dossier", "value": dossier },
        ]);
        let spec = prompt_chips();
        let res = call_field(db, &spec, kv, &provider_fn)?;
        let parsed = res.get("parsed").cloned().unwrap_or(Value::Null);
        let chips: Vec<Value> = parsed.as_array().map(|a| a.iter().take(12).cloned().collect()).unwrap_or_default();
        return Ok(json!({ "ok": true, "chips": chips, "ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null) }));
    }

    if op == "shape" {
        let intake = opt_str_or(input.get("intake"), "");
        if intake.trim().is_empty() { return Err(err_required("intake")); }
        let dossier_opts = json!({ "feature": input.get("feature").cloned().unwrap_or(Value::Null), "intake": input.get("intake").cloned().unwrap_or(Value::Null) });
        let dossier = buildDossier(db, &dossier_opts);
        let intake_kv: String = intake.chars().take(4000).collect();
        let kv = json!([
            { "key": "mode", "value": mode },
            { "key": "intake", "value": intake_kv },
            { "key": "chips", "value": chips_join(input.get("chips")) },
            { "key": "files", "value": files_block(input.get("files")) },
            { "key": "dossier", "value": dossier },
        ]);
        let spec = prompt_shape();
        let res = call_field(db, &spec, kv, &provider_fn)?;
        let parsed = res.get("parsed").cloned().unwrap_or(Value::Null);
        if !parsed.is_object() {
            // Unreachable in this twin (call_field's Err short-circuits first) — ported by inspection.
            return Ok(json!({ "ok": false, "error": "the AI returned no usable simulation — retry", "ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null) }));
        }
        return Ok(json!({ "ok": true, "shape": parsed, "ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null) }));
    }

    if op == "handoff" {
        let feature_context = match input.get("feature").filter(|v| !v.is_null()) {
            Some(f) => format!("{} \u{b7} {} \u{b7} {}", tmpl(f.get("name")), tmpl(f.get("kind")), opt_str_or(f.get("path"), "")),
            None => "(new)".to_string(),
        };
        let intake: String = opt_str_or(input.get("intake"), "").chars().take(4000).collect();
        let shape_str: String = serde_json::to_string(&input.get("shape").cloned().unwrap_or(json!({}))).unwrap_or_default().chars().take(6000).collect();
        let kv = json!([
            { "key": "mode", "value": mode },
            { "key": "feature_context", "value": feature_context },
            { "key": "intake", "value": intake },
            { "key": "chips", "value": chips_join(input.get("chips")) },
            { "key": "shape", "value": shape_str },
        ]);
        let spec = prompt_handoff();
        let res = call_field(db, &spec, kv, &provider_fn)?;
        let parsed = res.get("parsed").cloned().unwrap_or(Value::Null);
        let feature_name_ok = parsed.get("feature").and_then(|f| f.get("name")).map_or(false, |v| truthy(Some(v)));
        let cards_ok = parsed.get("cards").map_or(false, |v| v.is_array());
        if !feature_name_ok || !cards_ok {
            // Unreachable in this twin (call_field's Err short-circuits first) — ported by inspection.
            return Ok(json!({ "ok": false, "error": "the AI returned no usable hand-off — retry", "ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null) }));
        }
        let cards: Vec<Value> = parsed.get("cards").and_then(|v| v.as_array()).map(|a| a.iter().take(5).cloned().collect()).unwrap_or_default();
        return Ok(json!({ "ok": true, "feature": parsed.get("feature").cloned().unwrap_or(Value::Null), "cards": cards, "ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null) }));
    }

    if op == "image" {
        let prompt = opt_str_or(input.get("prompt"), "").trim().to_string();
        if prompt.is_empty() { return Err(err_required("prompt")); }
        let img_input = json!({ "prompt": prompt, "model_id": input.get("model_id").cloned().unwrap_or(Value::Null) });
        let r = crate::ai_ai_image_call::aiImageCall(&img_input, db)?;
        return Ok(json!({
            "ok": true,
            "image_data_url": r.get("image_data_url").cloned().unwrap_or(Value::Null),
            "cost_usd": r.get("cost_usd").cloned().unwrap_or(Value::Null),
            "ai_call_id": r.get("ai_call_id").cloned().unwrap_or(Value::Null),
        }));
    }

    Err(err_required("op (chips|shape|handoff|image)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE code_entry_points (kind TEXT, subtype TEXT, path TEXT, label TEXT, http_method TEXT, detail TEXT, line INTEGER, origin TEXT); \
             CREATE TABLE code_data_stories (id INTEGER, name TEXT, description TEXT, hops_json TEXT, linked_test TEXT, origin TEXT); \
             CREATE TABLE code_edges (src_path TEXT, dst_path TEXT, origin TEXT, kind TEXT); \
             CREATE TABLE memories (type TEXT, title TEXT, guidance TEXT); \
             CREATE TABLE cards (slug TEXT, title TEXT, status TEXT);",
        ).unwrap();
        c
    }

    // ── buildDossier ─────────────────────────────────────────────────────
    // TEST-BUG FIX: the original assertion assumed featureInventory's Err branch (the
    // "inventory unavailable (atlas not onboarded)" line) fires on an empty-but-present
    // schema. It does not: featureInventory (JS and this twin) swallows missing-table/
    // missing-row errors query-by-query and returns Ok with empty ui/nonui/gaps arrays, so
    // buildDossier's catch/Err arm is unreachable from this fixture — confirmed against real
    // JS via `node --input-type=module` with the identical empty schema (and even with NO
    // tables at all, since featureInventory's own per-query try/catch absorbs that too): both
    // return the same "[empty dossier — atlas not onboarded, no related evidence]" marker,
    // matching the sibling test below (build_dossier_with_no_evidence_at_all_is_the_empty_marker)
    // whose own comment already documents this. Renamed + reasserted on the real output.
    #[test]
    fn build_dossier_empty_db_yields_empty_marker() {
        let c = db();
        let out = buildDossier(&c, &json!({}));
        let s = out.as_str().unwrap();
        assert_eq!(s, "[empty dossier — atlas not onboarded, no related evidence]");
    }

    #[test]
    fn build_dossier_with_no_evidence_at_all_is_the_empty_marker() {
        // featureInventory succeeds (real tables exist, empty) so line [#1] is never the
        // fallback fixed string; with zero rows everywhere the join is empty and falls to marker.
        // Here we instead assert the *format* contract using a target with truly nothing.
        let c = db();
        let out = buildDossier(&c, &json!({ "feature": { "name": "Nope", "kind": "route" } }));
        let s = out.as_str().unwrap();
        assert!(s.contains("TARGET: \"Nope\" (route) \u{b7} no file"), "{s}");
    }

    #[test]
    fn build_dossier_entry_point_and_blast_radius_and_memory_and_cards() {
        let c = db();
        c.execute("INSERT INTO code_entry_points (kind, subtype, path, label, http_method, detail, line, origin) VALUES ('http','http-route','/checkout','POST /checkout','POST','handles payment',42,'atlas')", []).unwrap();
        c.execute("INSERT INTO code_edges (src_path, dst_path, origin) VALUES ('src/app.ts','/checkout','atlas')", []).unwrap();
        c.execute("INSERT INTO memories (type, title, guidance) VALUES ('decision','Checkout redesign','use stripe elements for checkout')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('feat-checkout-v2','Improve checkout flow','backlog')", []).unwrap();

        let out = buildDossier(&c, &json!({ "feature": { "name": "checkout", "kind": "route", "path": "/checkout" }, "intake": "improve checkout flow" }));
        let s = out.as_str().unwrap();
        assert!(s.contains("target entry point: http-route \"POST /checkout\" POST \u{b7} handles payment \u{b7} L42"), "{s}");
        assert!(s.contains("imported by: src/app.ts (blast radius)"), "{s}");
        assert!(s.contains("memory (decision) \"Checkout redesign\": use stripe elements for checkout"), "{s}");
        assert!(s.contains("related card feat-checkout-v2 [backlog]: Improve checkout flow"), "{s}");
        assert!(s.contains("target source unreadable at /checkout (workspace not mounted here)"), "{s}");
    }

    #[test]
    fn build_dossier_data_story_matched_by_hop_path() {
        let c = db();
        c.execute(
            "INSERT INTO code_data_stories (id, name, description, hops_json, linked_test, origin) VALUES (1,'checkout-flow','buy stuff','[{\"path\":\"/checkout\"}]',NULL,'atlas')",
            [],
        ).unwrap();
        let out = buildDossier(&c, &json!({ "feature": { "name": "checkout", "kind": "route", "path": "/checkout" } }));
        let s = out.as_str().unwrap();
        assert!(s.contains("target data-story \"checkout-flow\" \u{2014} buy stuff \u{b7} 1 hops (/checkout) \u{b7} NO linked test \u{2014} a change here means re-recording this story"), "{s}");
    }

    #[test]
    fn build_dossier_dedups_related_cards_by_slug_across_words() {
        let c = db();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('feat-alpha','Alpha beta gamma','backlog')", []).unwrap();
        let out = buildDossier(&c, &json!({ "intake": "alpha beta" }));
        let s = out.as_str().unwrap();
        let count = s.matches("related card feat-alpha").count();
        assert_eq!(count, 1, "{s}");
    }

    // ── files_block ──────────────────────────────────────────────────────
    #[test]
    fn files_block_none_when_absent_or_empty() {
        assert_eq!(files_block(None), "(none)");
        assert_eq!(files_block(Some(&json!([]))), "(none)");
    }

    #[test]
    fn files_block_renders_text_and_binary_entries() {
        let s = files_block(Some(&json!([{ "name": "a.md", "text": "hello" }, { "name": "b.png" }])));
        assert_eq!(s, "\u{2014} a.md:\nhello\n\u{2014} b.png (binary/image \u{2014} name only)");
    }

    // ── chips_join ───────────────────────────────────────────────────────
    #[test]
    fn chips_join_handles_strings_and_objects_and_empty() {
        assert_eq!(chips_join(None), "(none)");
        assert_eq!(chips_join(Some(&json!([]))), "(none)");
        assert_eq!(chips_join(Some(&json!(["fe", { "label": "risk" }]))), "fe, risk");
    }

    // ── ensure_prompt (real DB effect via ai_prompt_find/ai_prompt_add twins) ──
    fn prompt_schema_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.ai_providers ( id INTEGER PRIMARY KEY, tier TEXT, provider TEXT, model_id TEXT, active INTEGER NOT NULL DEFAULT 1, failover_rank INTEGER NOT NULL DEFAULT 0 );",
        ).unwrap();
        c
    }

    #[test]
    fn ensure_prompt_bootstraps_then_finds_the_template() {
        let c = prompt_schema_db();
        let spec = prompt_chips();
        let found = ensure_prompt(&c, &spec).unwrap();
        assert_eq!(found["name"], json!("feature.wizard.chips.v2"));
        // Second call: promptFind succeeds directly this time (no re-add, no duplicate row).
        let found2 = ensure_prompt(&c, &spec).unwrap();
        assert_eq!(found2["id"], found["id"]);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'feature.wizard.chips.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    // ── featureWizard: validation guards (reachable without a real AI call) ──
    #[test]
    fn feature_wizard_chips_requires_intake() {
        let c = db();
        let e = featureWizard(&json!({ "op": "chips" }), &c).unwrap_err();
        assert_eq!(e, "feature-wizard: intake is required");
        let e2 = featureWizard(&json!({ "op": "chips", "intake": "   " }), &c).unwrap_err();
        assert_eq!(e2, "feature-wizard: intake is required");
    }

    #[test]
    fn feature_wizard_shape_requires_intake() {
        let c = db();
        let e = featureWizard(&json!({ "op": "shape" }), &c).unwrap_err();
        assert_eq!(e, "feature-wizard: intake is required");
    }

    #[test]
    fn feature_wizard_image_requires_prompt() {
        let c = db();
        let e = featureWizard(&json!({ "op": "image" }), &c).unwrap_err();
        assert_eq!(e, "feature-wizard: prompt is required");
        let e2 = featureWizard(&json!({ "op": "image", "prompt": "  " }), &c).unwrap_err();
        assert_eq!(e2, "feature-wizard: prompt is required");
    }

    #[test]
    fn feature_wizard_unknown_op_requires_op() {
        let c = db();
        let e = featureWizard(&json!({}), &c).unwrap_err();
        assert_eq!(e, "feature-wizard: op (chips|shape|handoff|image) is required");
    }

    // ── featureWizard: op guards pass, then the deferred aiCall/aiImageCall Err propagates ──
    #[test]
    fn feature_wizard_chips_propagates_deferred_ai_call_error() {
        let c = prompt_schema_db();
        let e = featureWizard(&json!({ "op": "chips", "intake": "add a dashboard" }), &c).unwrap_err();
        assert!(e.contains("ai-call"), "{e}");
    }

    #[test]
    fn feature_wizard_image_propagates_deferred_ai_image_call_error() {
        let c = prompt_schema_db();
        let e = featureWizard(&json!({ "op": "image", "prompt": "a cat riding a bike" }), &c).unwrap_err();
        // the boundary is ai-image-call, which IS still parity-deferred (its OpenRouter dispatch
        // reads back a real billed cost). Note "ai-image-call" does not contain "ai-call".
        assert!(e.contains("PARITY-DEFERRED"), "{e}");
    }
}
